//! Dragging a file out of the download shelf. A page can't mint `File`
//! objects, so the shelf's HTML5 drag carries no real bytes — instead
//! the row's pointer-drag sends `download_drag` and the app starts a
//! genuine OLE drag here: `DoDragDrop` carrying a `CF_HDROP`. That is
//! what makes a drop on Explorer or the desktop copy the file, and a
//! drop back onto our own strip land as a file `DropEvent` the app can
//! open in a tab.

use std::path::Path;

/// Run the drag. Modal — returns when the user drops or cancels.
pub(super) fn drag_file(path: &Path) {
    #[cfg(target_os = "windows")]
    imp::run(path);
    #[cfg(not(target_os = "windows"))]
    let _ = path;
}

#[cfg(target_os = "windows")]
mod imp {
    use std::cell::Cell;
    use std::path::Path;

    use windows::core::{implement, Error, Interface, Result, BOOL, HRESULT};
    use windows::Win32::Foundation::{
        DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC,
        E_NOTIMPL, OLE_E_ADVISENOTSUPPORTED, RPC_E_CHANGED_MODE, S_FALSE, S_OK,
    };
    use windows::Win32::System::Com::{
        IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumFORMATETC_Impl,
        IEnumSTATDATA, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0,
        TYMED_HGLOBAL,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GHND};
    use windows::Win32::System::Ole::{
        DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, CF_HDROP, DROPEFFECT,
        DROPEFFECT_COPY, DROPEFFECT_NONE,
    };
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Shell::DROPFILES;

    /// `TYMED_HGLOBAL` in the `u32` width `FORMATETC`/`STGMEDIUM` carry.
    const TYMED_HGLOBAL_BITS: u32 = TYMED_HGLOBAL.0 as u32;

    /// The one format the object serves: an HDROP naming the file.
    const FILE_FORMAT: FORMATETC = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL_BITS,
    };

    /// `DROPFILES` + the double-NUL-terminated wide path list it points
    /// into — the exact bytes an HGLOBAL for CF_HDROP holds.
    fn hdrop_blob(path: &Path) -> Vec<u8> {
        use std::os::windows::ffi::OsStrExt;
        let header = std::mem::size_of::<DROPFILES>();
        let mut blob = Vec::with_capacity(header + 4);
        let df = DROPFILES {
            pFiles: header as u32,
            fWide: windows::core::BOOL(1),
            ..Default::default()
        };
        blob.extend_from_slice(unsafe {
            std::slice::from_raw_parts(&df as *const DROPFILES as *const u8, header)
        });
        for unit in path.as_os_str().encode_wide().chain([0, 0]) {
            blob.extend_from_slice(&unit.to_le_bytes());
        }
        blob
    }

    /// True when `fmt` asks for exactly what we serve.
    fn is_file_format(fmt: &FORMATETC) -> bool {
        fmt.cfFormat == CF_HDROP.0 && fmt.tymed & TYMED_HGLOBAL_BITS != 0
    }

    #[implement(IEnumFORMATETC)]
    struct FileFormatEnum {
        /// One format, served once — `Next` flips this after handing
        /// it out.
        given: Cell<bool>,
    }

    #[allow(non_snake_case)]
    impl IEnumFORMATETC_Impl for FileFormatEnum_Impl {
        fn Next(&self, celt: u32, rgelt: *mut FORMATETC, pceltfetched: *mut u32) -> HRESULT {
            unsafe {
                let mut fetched = 0;
                if celt > 0 && !rgelt.is_null() && !self.given.get() {
                    *rgelt = FILE_FORMAT;
                    self.given.set(true);
                    fetched = 1;
                }
                if !pceltfetched.is_null() {
                    *pceltfetched = fetched;
                }
                if fetched > 0 {
                    S_OK
                } else {
                    S_FALSE
                }
            }
        }

        fn Skip(&self, celt: u32) -> Result<()> {
            if celt > 0 {
                self.given.set(true);
            }
            Ok(())
        }

        fn Reset(&self) -> Result<()> {
            self.given.set(false);
            Ok(())
        }

        fn Clone(&self) -> Result<IEnumFORMATETC> {
            Ok(IEnumFORMATETC::from(FileFormatEnum {
                given: Cell::new(self.given.get()),
            }))
        }
    }

    /// The drag source and the payload in one object — `IDataObject`
    /// answers CF_HDROP, `IDropSource` decides when the drag ends.
    #[implement(IDataObject, IDropSource)]
    struct FileDrag {
        blob: Vec<u8>,
    }

    impl FileDrag {
        /// A fresh HGLOBAL copy of the payload — the callee owns it
        /// (`pUnkForRelease` null means ReleaseStgMedium frees the
        /// handle, which is what drop targets do).
        fn medium(&self) -> Result<STGMEDIUM> {
            unsafe {
                let handle = GlobalAlloc(GHND, self.blob.len())?;
                let dst = GlobalLock(handle);
                if dst.is_null() {
                    return Err(Error::from_thread());
                }
                std::ptr::copy_nonoverlapping(self.blob.as_ptr(), dst.cast(), self.blob.len());
                let _ = GlobalUnlock(handle);
                Ok(STGMEDIUM {
                    tymed: TYMED_HGLOBAL_BITS,
                    u: STGMEDIUM_0 { hGlobal: handle },
                    pUnkForRelease: std::mem::ManuallyDrop::new(None),
                })
            }
        }
    }

    #[allow(non_snake_case)]
    impl IDataObject_Impl for FileDrag_Impl {
        fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
            let fmt = unsafe { &*pformatetcin };
            if !is_file_format(fmt) {
                return Err(DV_E_FORMATETC.into());
            }
            self.medium()
        }

        fn GetDataHere(&self, pformatetc: *const FORMATETC, pmedium: *mut STGMEDIUM) -> Result<()> {
            let fmt = unsafe { &*pformatetc };
            if !is_file_format(fmt) || pmedium.is_null() {
                return Err(DV_E_FORMATETC.into());
            }
            let medium = self.medium()?;
            unsafe {
                (*pmedium).tymed = medium.tymed;
                (*pmedium).u.hGlobal = medium.u.hGlobal;
            }
            Ok(())
        }

        fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
            let fmt = unsafe { &*pformatetc };
            if is_file_format(fmt) {
                S_OK
            } else {
                DV_E_FORMATETC
            }
        }

        fn GetCanonicalFormatEtc(
            &self,
            _pformatectin: *const FORMATETC,
            _pformatetcout: *mut FORMATETC,
        ) -> HRESULT {
            E_NOTIMPL
        }

        fn SetData(
            &self,
            _pformatetc: *const FORMATETC,
            _pmedium: *const STGMEDIUM,
            _frelease: windows::core::BOOL,
        ) -> Result<()> {
            Err(E_NOTIMPL.into())
        }

        fn EnumFormatEtc(&self, dwdirection: u32) -> Result<IEnumFORMATETC> {
            if dwdirection == DATADIR_GET.0 as u32 {
                Ok(IEnumFORMATETC::from(FileFormatEnum {
                    given: Cell::new(false),
                }))
            } else {
                Err(E_NOTIMPL.into())
            }
        }

        fn DAdvise(
            &self,
            _pformatetc: *const FORMATETC,
            _advf: u32,
            _padvsink: windows::core::Ref<'_, IAdviseSink>,
        ) -> Result<u32> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn DUnadvise(&self, _dwconnection: u32) -> Result<()> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }
    }

    #[allow(non_snake_case)]
    impl IDropSource_Impl for FileDrag_Impl {
        fn QueryContinueDrag(
            &self,
            fescapepressed: BOOL,
            grfkeystate: MODIFIERKEYS_FLAGS,
        ) -> HRESULT {
            if fescapepressed.as_bool() {
                return DRAGDROP_S_CANCEL;
            }
            if grfkeystate.0 & MK_LBUTTON.0 == 0 {
                return DRAGDROP_S_DROP;
            }
            S_OK
        }

        fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    /// Modal OLE drag of `path`, copy-only. Called on the event loop
    /// thread while the shelf's button is still held — `DoDragDrop`
    /// pumps its own message loop, so the window stays live and drops
    /// on our own views still land.
    pub(super) fn run(path: &Path) {
        unsafe {
            // Already initialized on this thread — S_FALSE counts as
            // success. A mismatched apartment (RPC_E_CHANGED_MODE) is
            // the expected Err and is ignored: the drag still runs on
            // the ambient one. Anything else means OLE is not up and
            // DoDragDrop will fail too, so report it and try anyway.
            if let Err(e) = OleInitialize(None) {
                if e.code() != RPC_E_CHANGED_MODE {
                    crate::report!("drag-out: OleInitialize failed: {e}");
                }
            }
            let data: IDataObject = FileDrag {
                blob: hdrop_blob(path),
            }
            .into();
            let source = data
                .cast::<IDropSource>()
                .expect("FileDrag implements IDropSource");
            let mut effect = DROPEFFECT_NONE;
            // Drop and cancel come back as success codes; only a real
            // failure is worth reporting.
            if let Err(e) = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect).ok() {
                crate::report!("drag-out: DoDragDrop failed: {e}");
            }
        }
    }
}
