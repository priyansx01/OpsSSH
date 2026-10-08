use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

#[derive(Debug)]
pub(super) struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if path == "branding/opsssh.png" {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/branding/opsssh.png"
            ))));
        }
        gpui_kit_assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut assets = gpui_kit_assets::AllAssets.list(path)?;
        if "branding/opsssh.png".starts_with(path) {
            assets.push("branding/opsssh.png".into());
        }
        Ok(assets)
    }
}
