use std::{
    fs::File,
    io::{self, Read, Write},
    path::PathBuf,
    time::{Duration, Instant},
};
use url::Url;

const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const DEADLINE: Duration = Duration::from_secs(90);

pub struct DownloadedPdf {
    pub path: PathBuf,
    _directory: tempfile::TempDir,
}

pub fn parse(text: &str) -> Result<Url, &'static str> {
    let text = text.trim();
    if text.len() > 8192
        || !text.split_once("://").is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        })
    {
        return Err("Enter a complete link beginning with https:// or http://.");
    }
    let mut url = Url::parse(text).map_err(|_| "Enter a valid PDF link.")?;
    validate(&url)?;
    url.set_fragment(None);
    Ok(url)
}

fn validate(url: &Url) -> Result<(), &'static str> {
    if !matches!(url.scheme(), "http" | "https") || !url.has_host() {
        return Err("Only HTTP and HTTPS links can be opened.");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Remove the username or password from the link before opening it.");
    }
    Ok(())
}

fn redirect(from: &Url, destination: &str) -> Result<Url, &'static str> {
    let mut next = from
        .join(destination)
        .map_err(|_| "The server returned an invalid redirect.")?;
    validate(&next)?;
    if from.scheme() == "https" && next.scheme() != "https" {
        return Err("The server redirected the secure link to an unsecured connection.");
    }
    next.set_fragment(None);
    Ok(next)
}

pub fn download(mut url: Url) -> Result<DownloadedPdf, String> {
    let started = Instant::now();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(12)))
        .http_status_as_error(false)
        .build()
        .into();
    for redirects in 0..=MAX_REDIRECTS {
        let remaining = DEADLINE.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err("The download timed out. Try again.".into());
        }
        let mut response = agent
            .get(url.as_str())
            .header("Accept", "application/pdf, application/octet-stream;q=0.9")
            .config()
            .timeout_global(Some(remaining))
            .build()
            .call()
            .map_err(|error| network_error(&error))?;
        match response.status().as_u16() {
            301 | 302 | 303 | 307 | 308 if redirects < MAX_REDIRECTS => {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .ok_or("The server returned a redirect without a valid destination.")?;
                url = redirect(&url, location)?;
            }
            301 | 302 | 303 | 307 | 308 => return Err("The link redirected too many times.".into()),
            200 => {
                if response
                    .headers()
                    .get("content-length")
                    .and_then(|header| header.to_str().ok())
                    .and_then(|length| length.parse::<u64>().ok())
                    .is_some_and(|bytes| bytes > MAX_BYTES)
                {
                    return Err("The PDF exceeds the 128 MiB download limit.".into());
                }
                let directory = tempfile::Builder::new()
                    .prefix("TinyPDF-link-")
                    .tempdir()
                    .map_err(|error| format!("Could not create temporary PDF storage. {error}"))?;
                let name = url
                    .path_segments()
                    .and_then(|segments| segments.last())
                    .filter(|name| {
                        name.len() <= 100
                            && name.to_ascii_lowercase().ends_with(".pdf")
                            && name
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
                    });
                let filename =
                    name.map_or_else(|| "Web document.pdf".into(), |name| format!("Web - {name}"));
                let path = directory.path().join(filename);
                let mut file = File::create(&path)
                    .map_err(|error| format!("Could not create the temporary PDF. {error}"))?;
                let reader = response.body_mut().as_reader();
                copy_pdf(reader, &mut file, MAX_BYTES)?;
                file.flush().map_err(|error| {
                    format!("Could not finish writing the temporary PDF. {error}")
                })?;
                return Ok(DownloadedPdf {
                    path,
                    _directory: directory,
                });
            }
            code => {
                return Err(format!(
                    "The server returned HTTP {code}. Check the link or your access."
                ));
            }
        }
    }
    unreachable!("Every final response returns")
}

fn network_error(error: &ureq::Error) -> String {
    // Network diagnostics must not include URLs containing private access tokens.
    if matches!(error, ureq::Error::Timeout(_)) {
        "The download timed out. Try again.".into()
    } else {
        "Could not connect to the PDF. Check your connection and the link’s certificate.".into()
    }
}

fn copy_pdf(reader: impl Read, mut writer: impl Write, limit: u64) -> Result<(), String> {
    let mut bounded = reader.take(limit + 1);
    let mut header = Vec::new();
    bounded
        .by_ref()
        .take(1024)
        .read_to_end(&mut header)
        .map_err(|error| format!("Could not read the PDF response. {error}"))?;
    if !header.windows(5).any(|marker| marker == b"%PDF-") {
        return Err("This link returned something other than a PDF.".into());
    }
    writer
        .write_all(&header)
        .map_err(|error| format!("Could not write the temporary PDF. {error}"))?;
    let bytes = io::copy(&mut bounded, &mut writer)
        .map_err(|error| format!("Could not finish downloading the PDF. {error}"))?;
    if header.len() as u64 + bytes > limit {
        return Err("The PDF exceeds the 128 MiB download limit.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_local_schemes_credentials_and_insecure_redirects() {
        for invalid in [
            "file:///C:/a.pdf",
            "https://user:password@example.test/a.pdf",
            "http://",
        ] {
            assert!(parse(invalid).is_err());
        }
        let link = parse(" https://example.test/one.pdf#page=3 ").unwrap();
        assert_eq!(link.as_str(), "https://example.test/one.pdf");
        assert!(redirect(&link, "http://example.test/two.pdf").is_err());
        assert!(redirect(&link, "file:///C:/a.pdf").is_err());
        assert_eq!(redirect(&link, "/two.pdf").unwrap().path(), "/two.pdf");
    }

    #[test]
    fn pdf_stream_checks_the_header_and_enforces_the_limit() {
        let pdf = b"%PDF-1.7\ncontent";
        let mut copy = Vec::new();
        copy_pdf(&pdf[..], &mut copy, pdf.len() as u64).unwrap();
        assert_eq!(copy, pdf);
        assert!(copy_pdf(&pdf[..], io::sink(), pdf.len() as u64 - 1).is_err());
        assert!(copy_pdf(&b"<html>sign in</html>"[..], io::sink(), 1024).is_err());
    }
}
