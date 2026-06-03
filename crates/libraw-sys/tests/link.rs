//! Confirms the vendored/system LibRaw links and the C API is callable:
//! initialize a context, query the version string, and recycle it.

#[test]
fn init_version_and_close() {
    unsafe {
        let ctx = libraw_sys::libraw_init(0);
        assert!(!ctx.is_null(), "libraw_init returned null");

        let ver = libraw_sys::libraw_version();
        assert!(!ver.is_null(), "libraw_version returned null");
        let ver = std::ffi::CStr::from_ptr(ver).to_string_lossy();
        assert!(
            ver.starts_with("0.22"),
            "unexpected LibRaw version: {ver}"
        );

        libraw_sys::libraw_close(ctx);
    }
}
