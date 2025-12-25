use anyhow::Context;
use sanitize_filename::sanitize;
use std::path::{Path, PathBuf};
use url::Url;

pub fn filename_from_headers_and_url(
  url: &Url,
  content_disposition: Option<&str>,
  content_type: Option<&str>,
) -> String {
  if let Some(cd) = content_disposition {
    if let Some(n) = parse_content_disposition_filename(cd) {
      return sanitize(decode_filename_like(&n));
    }
  }

  if let Some(seg) = url
    .path_segments()
    .and_then(|s| s.last())
    .filter(|s| !s.is_empty())
  {
    let decoded = decode_filename_like(seg);
    let s = sanitize(decoded);
    if !s.is_empty() && s != "." {
      return s;
    }
  }

  let mut base = "download".to_string();
  if let Some(ct) = content_type {
    if let Some(ext) = mime_guess::get_mime_extensions_str(ct)
      .and_then(|exts| exts.first().copied())
    {
      base.push('.');
      base.push_str(ext);
    }
  }
  base
}

fn parse_content_disposition_filename(cd: &str) -> Option<String> {
  // Supports common forms:
  // - filename="a.txt"
  // - filename=a.txt
  // - filename*=UTF-8''a%20b.txt
  let cd = cd.trim();

  fn take_param_value(s: &str) -> &str {
    // Extract the parameter value up to the next ';' (unless that ';' occurs inside quotes).
    // This avoids accidentally consuming subsequent parameters like `; filename=...`.
    let mut in_quotes = false;
    let mut escape = false;
    for (i, ch) in s.char_indices() {
      if escape {
        escape = false;
        continue;
      }
      match ch {
        '\\' if in_quotes => escape = true,
        '"' => in_quotes = !in_quotes,
        ';' if !in_quotes => return s[..i].trim(),
        _ => {}
      }
    }
    s.trim()
  }

  // filename*=
  if let Some(idx) = cd.to_ascii_lowercase().find("filename*=") {
    let rest = &cd[idx + "filename*=".len()..];
    let rest = take_param_value(rest.trim_start());
    // Often: UTF-8''... (RFC 5987)
    if let Some(pos) = rest.find("''") {
      let enc_value = &rest[pos + 2..];
      let enc_value = enc_value.trim().trim_matches('"');
      if let Ok(decoded) = urlencoding::decode(enc_value) {
        return Some(decoded.into_owned());
      }
    }
    let value = rest.trim().trim_matches('"');
    if !value.is_empty() {
      return Some(value.to_string());
    }
  }

  // filename=
  if let Some(idx) = cd.to_ascii_lowercase().find("filename=") {
    let mut rest = &cd[idx + "filename=".len()..];
    // stop at ';'
    if let Some(semi) = rest.find(';') {
      rest = &rest[..semi];
    }
    let value = rest.trim().trim_matches('"');
    if !value.is_empty() {
      return Some(decode_filename_like(value));
    }
  }

  None
}

fn decode_filename_like(s: &str) -> String {
  // Best-effort decode:
  // - Content-Disposition sometimes contains percent-escapes (e.g. %20) even in filename=.
  // - URL path segments may also be percent-encoded.
  //
  // urlencoding::decode is good enough here; it also handles '+' which some servers misuse.
  match urlencoding::decode(s) {
    Ok(v) => v.into_owned(),
    Err(_) => s.replace("%20", " "),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn content_disposition_filename_star_does_not_consume_following_params() {
    let cd = "attachment; filename*=UTF-8''Qwen3-4B-Q5_K_M.gguf; filename=Qwen3-4B-Q5_K_M.gguf";
    let got = parse_content_disposition_filename(cd).unwrap();
    assert_eq!(got, "Qwen3-4B-Q5_K_M.gguf");
  }

  #[test]
  fn content_disposition_filename_basic() {
    let cd = r#"attachment; filename="Qwen3-4B-Q5_K_M.gguf""#;
    let got = parse_content_disposition_filename(cd).unwrap();
    assert_eq!(got, "Qwen3-4B-Q5_K_M.gguf");
  }

  #[test]
  fn content_disposition_filename_star_percent_decodes() {
    let cd = "attachment; filename*=UTF-8''a%20b.txt; filename=a b.txt";
    let got = parse_content_disposition_filename(cd).unwrap();
    assert_eq!(got, "a b.txt");
  }
}

pub fn choose_non_colliding_filename(dest_dir: &Path, desired: &str) -> anyhow::Result<String> {
  let desired = sanitize(desired);
  let desired = if desired.is_empty() { "download".to_string() } else { desired };

  let mut candidate = desired.clone();
  let mut n = 1;
  loop {
    let p = dest_dir.join(&candidate);
    if !p.exists() {
      return Ok(candidate);
    }
    candidate = append_suffix(&desired, n);
    n += 1;
    if n > 10_000 {
      anyhow::bail!("too many filename collisions");
    }
  }
}

fn append_suffix(original: &str, n: usize) -> String {
  // "file.ext" => "file (n).ext"
  // "file" => "file (n)"
  let p = PathBuf::from(original);
  let stem = p
    .file_stem()
    .and_then(|s| s.to_str())
    .unwrap_or(original);
  let ext = p.extension().and_then(|s| s.to_str());
  if let Some(ext) = ext {
    format!("{stem} ({n}).{ext}")
  } else {
    format!("{stem} ({n})")
  }
}

pub fn ensure_dir(dest_dir: &Path) -> anyhow::Result<()> {
  std::fs::create_dir_all(dest_dir).context("failed to create destination dir")
}


