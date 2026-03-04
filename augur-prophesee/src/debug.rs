pub(crate) fn enabled() -> bool {
    std::env::var("CAMERA_DEBUG")
        .map(|value| {
            let value = value.trim();
            !value.is_empty()
                && !value.eq_ignore_ascii_case("0")
                && !value.eq_ignore_ascii_case("false")
                && !value.eq_ignore_ascii_case("no")
        })
        .unwrap_or(false)
}

pub(crate) fn log(message: impl AsRef<str>) {
    if enabled() {
        eprintln!("[camera-debug] {}", message.as_ref());
    }
}
