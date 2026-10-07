//! Native SSH agent selection. Secrets and key material remain in the agent.
use russh::keys::agent::client::{AgentClient, AgentStream};
pub type DynamicAgent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;
pub async fn connect_agent(
    identity_agent: Option<&str>,
) -> Result<DynamicAgent, russh::keys::Error> {
    #[cfg(windows)]
    {
        if identity_agent == Some("pageant") {
            return Ok(AgentClient::connect_pageant().await?.dynamic());
        }
        let path = identity_agent
            .map(str::to_owned)
            .or_else(|| std::env::var("SSH_AUTH_SOCK").ok())
            .unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".to_owned());
        Ok(AgentClient::connect_named_pipe(path).await?.dynamic())
    }
    #[cfg(unix)]
    {
        if let Some(path) = identity_agent {
            Ok(AgentClient::connect_uds(path).await?.dynamic())
        } else {
            Ok(AgentClient::connect_env().await?.dynamic())
        }
    }
}
