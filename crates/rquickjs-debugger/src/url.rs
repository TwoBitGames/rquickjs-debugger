#[must_use]
pub fn script_url(name: &str) -> String {
    if name.contains("://") {
        return name.to_string();
    }
    let path = name.replace('\\', "/");
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    format!("file://{}", percent_encode(&path))
}

#[must_use]
pub(crate) fn script_name(url: &str) -> String {
    let Some(rest) = url.strip_prefix("file://") else {
        return if url.contains("://") {
            url.to_string()
        } else {
            String::new()
        };
    };
    let decoded = percent_decode(rest);
    if cfg!(windows) {
        windows_path(&decoded)
    } else {
        decoded
    }
}

fn windows_path(decoded: &str) -> String {
    let bytes = decoded.as_bytes();
    let has_drive = bytes.len() > 2 && bytes[0] == b'/' && bytes[2] == b':';
    let trimmed = if has_drive { &decoded[1..] } else { decoded };
    trimmed.replace('/', "\\")
}

fn percent_encode(s: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
