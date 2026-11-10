//! Outbound file drags to the Windows shell (Explorer, the desktop, other apps).
//!
//! A drop onto an Explorer window or the desktop is not completed by the shell: the
//! drag is cancelled at release and the caller downloads into the folder under the
//! pointer, so large files need no preparation. Other applications receive real files
//! through the shell's own data object, so only already-prepared files can go there.
use std::path::{Path, PathBuf};

/// What happened when the user released the drag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragOutcome {
    /// Released over an Explorer folder or the desktop: download into this folder.
    Folder(PathBuf),
    /// Another application received the prepared files.
    Dropped,
    /// Released over another application before the files were prepared.
    NeedsPreparedFiles,
    Cancelled,
}

/// Run a native drag of local files that share one folder. `files_ready` says whether
/// they are real copies other applications may receive, rather than empty stand-ins
/// used only for the drag's appearance. Call on the UI thread while the button is held.
pub fn start_file_drag(paths: &[PathBuf], files_ready: bool) -> Result<DragOutcome, String> {
    let parent = shared_parent(paths)?;
    if !parent.is_dir() || paths.iter().any(|path| !path.exists()) {
        return Err("Prepared files are no longer available".into());
    }
    native::start_file_drag(paths, files_ready)
}

/// The shell's item array requires every item to come from the same folder.
fn shared_parent(paths: &[PathBuf]) -> Result<&Path, String> {
    let first = paths
        .first()
        .and_then(|path| path.parent())
        .ok_or("Nothing to drag")?;
    if paths.iter().any(|path| path.parent() != Some(first)) {
        return Err("Dragged files must share one folder".into());
    }
    Ok(first)
}

/// Where the pointer was when the button was released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Release {
    #[default]
    Escaped,
    Explorer(isize),
    Desktop,
    OwnWindow,
    Other,
}

/// Window classes of Explorer folder windows and of the desktop's icon host.
fn classify(class: &str) -> Option<Release> {
    match class {
        "CabinetWClass" | "ExploreWClass" => Some(Release::Explorer(0)),
        "Progman" | "WorkerW" => Some(Release::Desktop),
        _ => None,
    }
}

// Audited Win32 boundary: every unsafe block documents its invariants.
#[allow(unsafe_code)]
mod native {
    use super::{DragOutcome, Release, classify};
    use std::{cell::Cell, mem::ManuallyDrop, os::windows::ffi::OsStrExt, path::PathBuf, rc::Rc};
    use windows::{
        Win32::{
            Foundation::{
                DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, POINT, S_OK,
            },
            System::{
                Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, IDataObject, IServiceProvider},
                Ole::{DROPEFFECT, DROPEFFECT_COPY, IDropSource, IDropSource_Impl},
                SystemServices::{MK_LBUTTON, MK_MBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS},
                Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4},
            },
            UI::{
                Shell::{
                    BHID_DataObject, Common::ITEMIDLIST, FOLDERID_Desktop, IFolderView,
                    ILCreateFromPathW, ILFree, IPersistFolder2, IShellBrowser, IShellItemArray,
                    IShellWindows, IWebBrowserApp, KF_FLAG_DEFAULT,
                    SHCreateShellItemArrayFromIDLists, SHDoDragDrop, SHGetKnownFolderPath,
                    SHGetPathFromIDListW, SID_STopLevelBrowser, ShellWindows,
                },
                WindowsAndMessaging::{
                    GA_ROOT, GetAncestor, GetClassNameW, GetCursorPos, GetWindowThreadProcessId,
                    IsWindowVisible, WindowFromPoint,
                },
            },
        },
        core::{BOOL, HRESULT, Interface, PCWSTR, implement},
    };

    /// Decides at release whether the shell should complete the drop.
    #[implement(IDropSource)]
    struct DropSource {
        files_ready: bool,
        release: Rc<Cell<Release>>,
    }

    impl IDropSource_Impl for DropSource_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape.as_bool() {
                self.release.set(Release::Escaped);
                return DRAGDROP_S_CANCEL;
            }
            if keys.0 & (MK_LBUTTON.0 | MK_RBUTTON.0 | MK_MBUTTON.0) != 0 {
                return S_OK;
            }
            let release = release_target();
            self.release.set(release);
            match release {
                // OpsSSH downloads into these itself; the stand-ins are never copied.
                Release::Explorer(_) | Release::Desktop => DRAGDROP_S_CANCEL,
                // Never feed OpsSSH's own drop handling (it would upload the files).
                Release::OwnWindow | Release::Escaped => DRAGDROP_S_CANCEL,
                Release::Other if self.files_ready => DRAGDROP_S_DROP,
                Release::Other => DRAGDROP_S_CANCEL,
            }
        }
        fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    fn release_target() -> Release {
        let mut point = POINT::default();
        // SAFETY: `point` is a valid out-pointer; the remaining calls take window handles
        // returned by Windows and a correctly sized UTF-16 buffer.
        unsafe {
            if GetCursorPos(&mut point).is_err() {
                return Release::Other;
            }
            let window = WindowFromPoint(point);
            if window.is_invalid() {
                return Release::Other;
            }
            let root = GetAncestor(window, GA_ROOT);
            let mut process = 0u32;
            GetWindowThreadProcessId(root, Some(&mut process));
            if process == std::process::id() {
                return Release::OwnWindow;
            }
            let mut class = [0u16; 64];
            let length = GetClassNameW(root, &mut class).max(0) as usize;
            match classify(&String::from_utf16_lossy(&class[..length])) {
                Some(Release::Explorer(_)) => Release::Explorer(root.0 as isize),
                Some(release) => release,
                None => Release::Other,
            }
        }
    }

    /// Owns absolute item ID lists and frees them on every exit path.
    struct IdLists(Vec<*mut ITEMIDLIST>);
    impl Drop for IdLists {
        fn drop(&mut self) {
            for list in self.0.drain(..) {
                // SAFETY: each pointer came from ILCreateFromPathW, is non-null, and is
                // freed exactly once here.
                unsafe { ILFree(Some(list)) };
            }
        }
    }

    pub fn start_file_drag(paths: &[PathBuf], files_ready: bool) -> Result<DragOutcome, String> {
        let mut lists = IdLists(Vec::with_capacity(paths.len()));
        for path in paths {
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call.
            let list = unsafe { ILCreateFromPathW(PCWSTR(wide.as_ptr())) };
            if list.is_null() {
                return Err(format!("Windows could not prepare {}", path.display()));
            }
            lists.0.push(list);
        }
        let borrowed: Vec<*const ITEMIDLIST> =
            lists.0.iter().map(|list| list.cast_const()).collect();
        // SAFETY: every pointer is a live absolute ID list owned by `lists`, which outlives
        // the shell array creation; the shell copies what it keeps.
        let items: IShellItemArray = unsafe { SHCreateShellItemArrayFromIDLists(&borrowed) }
            .map_err(|error| format!("Windows could not prepare the files: {error}"))?;
        // SAFETY: a COM call on a valid interface; the shell returns its own data object.
        let data: IDataObject = unsafe { items.BindToHandler(None, &BHID_DataObject) }
            .map_err(|error| format!("Windows could not offer the files: {error}"))?;
        let release = Rc::new(Cell::new(Release::Escaped));
        let source: IDropSource = DropSource {
            files_ready,
            release: release.clone(),
        }
        .into();
        // SAFETY: called on the UI thread, where GPUI initialized OLE. The call runs a
        // modal drag loop and keeps both interfaces alive until it returns.
        let result = unsafe { SHDoDragDrop(None, &data, &source, DROPEFFECT_COPY) };
        match release.get() {
            Release::Explorer(window) => explorer_folder(HWND(window as *mut _))
                .map(DragOutcome::Folder)
                .ok_or_else(|| "That Explorer location is not a folder on this PC".into()),
            Release::Desktop => desktop_folder().map(DragOutcome::Folder),
            Release::Other if files_ready => match result {
                Ok(effect) if effect.0 != 0 => Ok(DragOutcome::Dropped),
                Ok(_) => Ok(DragOutcome::Cancelled),
                Err(error) => Err(format!("The drag could not finish: {error}")),
            },
            Release::Other => Ok(DragOutcome::NeedsPreparedFiles),
            Release::OwnWindow | Release::Escaped => Ok(DragOutcome::Cancelled),
        }
    }

    fn variant_i4(value: i32) -> VARIANT {
        VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                    vt: VT_I4,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: VARIANT_0_0_0 { lVal: value },
                }),
            },
        }
    }

    /// The folder shown in an Explorer window. Windows 11 tabs share one frame window;
    /// the visible tab is the active one.
    fn explorer_folder(frame: HWND) -> Option<PathBuf> {
        // SAFETY: COM calls on interfaces returned by the shell; every returned ID list
        // is freed with CoTaskMemFree and the path buffer is a fixed MAX_PATH array.
        unsafe {
            let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;
            let mut fallback = None;
            for index in 0..windows.Count().ok()? {
                let Ok(dispatch) = windows.Item(&variant_i4(index)) else {
                    continue;
                };
                let Ok(browser_app) = dispatch.cast::<IWebBrowserApp>() else {
                    continue;
                };
                if browser_app.HWND().ok().map(|handle| handle.0) != Some(frame.0 as isize) {
                    continue;
                }
                let Ok(provider) = browser_app.cast::<IServiceProvider>() else {
                    continue;
                };
                let Ok(browser) = provider.QueryService::<IShellBrowser>(&SID_STopLevelBrowser)
                else {
                    continue;
                };
                let Some(path) = browser_folder(&browser) else {
                    continue;
                };
                let active = browser
                    .GetWindow()
                    .is_ok_and(|tab| IsWindowVisible(tab).as_bool());
                if active {
                    return Some(path);
                }
                fallback.get_or_insert(path);
            }
            fallback
        }
    }

    unsafe fn browser_folder(browser: &IShellBrowser) -> Option<PathBuf> {
        // SAFETY: see `explorer_folder`; the caller passes a live shell browser.
        unsafe {
            let view = browser.QueryActiveShellView().ok()?;
            let folder_view = view.cast::<IFolderView>().ok()?;
            let folder: IPersistFolder2 = folder_view.GetFolder().ok()?;
            let list = folder.GetCurFolder().ok()?;
            let mut buffer = [0u16; 260];
            let found = SHGetPathFromIDListW(list, &mut buffer).as_bool();
            CoTaskMemFree(Some(list as *const _));
            if !found {
                return None;
            }
            let length = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
            Some(PathBuf::from(String::from_utf16_lossy(&buffer[..length])))
        }
    }

    fn desktop_folder() -> Result<PathBuf, String> {
        // SAFETY: the returned string is read once and freed with CoTaskMemFree.
        unsafe {
            let path = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None)
                .map_err(|error| format!("The desktop folder is unavailable: {error}"))?;
            let text = path.to_string();
            CoTaskMemFree(Some(path.0 as *const _));
            text.map(PathBuf::from)
                .map_err(|_| "The desktop path is not valid Unicode".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragged_files_must_share_one_existing_folder() {
        let dir = std::env::temp_dir();
        assert!(shared_parent(&[]).is_err());
        assert!(shared_parent(&[dir.join("a"), dir.join("b")]).is_ok());
        assert!(shared_parent(&[dir.join("a"), dir.join("sub").join("b")]).is_err());
        assert!(start_file_drag(&[dir.join("opsssh-missing-drag-file")], false).is_err());
    }

    #[test]
    fn explorer_and_desktop_windows_are_recognized() {
        assert_eq!(classify("CabinetWClass"), Some(Release::Explorer(0)));
        assert_eq!(classify("Progman"), Some(Release::Desktop));
        assert_eq!(classify("WorkerW"), Some(Release::Desktop));
        assert_eq!(classify("Chrome_WidgetWin_1"), None);
    }
}
