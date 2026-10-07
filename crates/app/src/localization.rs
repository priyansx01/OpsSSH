//! Embedded Fluent catalog. UI lookups are cached per UI thread and fall back to message IDs.
use fluent_bundle::{FluentBundle, FluentResource};
thread_local! {
    static ENGLISH:FluentBundle<FluentResource> = {
        let language:unic_langid::LanguageIdentifier="en-US".parse().expect("valid built-in locale");
        let mut bundle=FluentBundle::new(vec![language]);
        let resource=FluentResource::try_new(include_str!("../../../i18n/en-US.ftl").into()).expect("valid embedded Fluent catalog");
        bundle.add_resource(resource).expect("unique Fluent message IDs");bundle
    };
}
pub fn text(key: &str) -> String {
    ENGLISH.with(|bundle| {
        let Some(pattern) = bundle.get_message(key).and_then(|message| message.value()) else {
            return key.into();
        };
        bundle
            .format_pattern(pattern, None, &mut vec![])
            .into_owned()
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_loads_and_missing_ids_fall_back() {
        assert_eq!(text("app-name"), "OpsSSH");
        assert_eq!(text("unknown-key"), "unknown-key");
    }
}
