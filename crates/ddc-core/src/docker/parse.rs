//! `docker system df -v --format '{{json .}}'`, read: every value is a
//! string docker's formatter made. A size is a decimal one (`units.HumanSize`:
//! three or four significant digits, `1e+03kB` included), or `N/A` when
//! docker does not know it. Anything else is unreadable, never zero.

use serde::Deserialize;

use super::{DockerResult, Estimate, Group};

/// A size string docker would not print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeError(pub String);

impl std::fmt::Display for SizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not a size docker prints: {:?}", self.0)
    }
}

/// Docker's decimal units, largest first by length so `kB` is tried before `B`.
const UNITS: [(&str, i32); 7] = [("kB", 1), ("MB", 2), ("GB", 3), ("TB", 4), ("PB", 5), ("EB", 6), ("B", 0)];

/// A size docker printed, in bytes; `None` for `N/A`.
pub fn size(text: &str) -> Result<Option<u64>, SizeError> {
    if text == "N/A" {
        return Ok(None);
    }
    let err = || SizeError(text.to_owned());
    let (number, power) =
        UNITS.iter().find_map(|(unit, power)| text.strip_suffix(unit).map(|n| (n, *power))).ok_or_else(err)?;
    if !decimal(number) {
        return Err(err());
    }
    let value: f64 = number.parse().map_err(|_| err())?;
    let bytes = (value * 1000f64.powi(power)).round();
    // 2^64 itself is not a u64.
    if !bytes.is_finite() || bytes < 0.0 || bytes >= 18_446_744_073_709_551_616.0 {
        return Err(err());
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked just above
    Ok(Some(bytes as u64))
}

/// `\d+(\.\d+)?([eE][+-]?\d+)?`, what Go's `%.4g` prints for a size.
fn decimal(s: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (mantissa, exponent) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((int, frac)) => digits(int) && digits(frac),
        None => digits(mantissa),
    };
    let exponent_ok = exponent.is_none_or(|e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)));
    mantissa_ok && exponent_ok
}

/// A `"true"` or `"false"` docker printed.
fn flag(text: &str) -> Result<bool, String> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("not a flag docker prints: {other:?}")),
    }
}

/// The fields read; docker prints more, and those are left alone.
#[derive(Deserialize)]
struct Df {
    #[serde(rename = "Images")]
    images: Option<Vec<Image>>,
    #[serde(rename = "Containers")]
    containers: Option<Vec<Sized>>,
    #[serde(rename = "Volumes")]
    volumes: Option<Vec<Sized>>,
    #[serde(rename = "BuildCache")]
    build_cache: Option<Vec<Cache>>,
}

#[derive(Deserialize)]
struct Image {
    #[serde(rename = "Repository")]
    repository: String,
    #[serde(rename = "Tag")]
    tag: String,
    #[serde(rename = "Containers")]
    containers: String,
    #[serde(rename = "UniqueSize")]
    unique_size: String,
}

#[derive(Deserialize)]
struct Sized {
    #[serde(rename = "Size")]
    size: String,
}

#[derive(Deserialize)]
struct Cache {
    #[serde(rename = "CacheType")]
    cache_type: String,
    #[serde(rename = "InUse")]
    in_use: String,
    #[serde(rename = "Shared")]
    shared: String,
    #[serde(rename = "Size")]
    size: String,
}

/// `system df -v`'s output, read; anything ddc cannot read is `unreadable`.
pub(super) fn listed(stdout: &str) -> DockerResult {
    read(stdout).unwrap_or_else(|detail| DockerResult::Unreadable { detail })
}

fn read(stdout: &str) -> Result<DockerResult, String> {
    let df: Df =
        serde_json::from_str(stdout.trim()).map_err(|e| format!("system df -v printed no report ddc reads: {e}"))?;
    let images = df.images.unwrap_or_default();
    let containers = df.containers.unwrap_or_default();
    let volumes = df.volumes.unwrap_or_default();
    let build_cache = df.build_cache.unwrap_or_default();
    let sized = |s: &str| size(s).map_err(|e| e.to_string());

    let mut image_group = Group { count: images.len() as u64, bytes: Some(0) };
    let mut dangling = Some(0u64);
    for image in &images {
        let unique = sized(&image.unique_size)?;
        image_group.bytes = add(image_group.bytes, unique);
        if image.repository == "<none>" && image.tag == "<none>" && image.containers == "0" {
            dangling = add(dangling, unique);
        }
    }

    let mut container_group = Group { count: containers.len() as u64, bytes: Some(0) };
    for c in &containers {
        // `-v` prints the writable layer alone; a virtual size is cut, to be safe.
        let own = c.size.split_once(" (virtual").map_or(c.size.as_str(), |(own, _)| own);
        container_group.bytes = add(container_group.bytes, sized(own)?);
    }

    let mut volume_group = Group { count: volumes.len() as u64, bytes: Some(0) };
    for v in &volumes {
        // A volume docker could not size is left out, as `docker system df`
        // leaves it out: `N/A`, or `-1B` when moby failed to measure its folder.
        if v.size == "-1B" {
            continue;
        }
        if let Some(bytes) = sized(&v.size)? {
            volume_group.bytes = add(volume_group.bytes, Some(bytes));
        }
    }

    let mut cache_group = Group { count: build_cache.len() as u64, bytes: Some(0) };
    let mut prunable = 0u64;
    for record in &build_cache {
        let bytes = sized(&record.size)?.ok_or("a build-cache record without a size")?;
        let shared = flag(&record.shared)?;
        let in_use = flag(&record.in_use)?;
        if !shared {
            cache_group.bytes = add(cache_group.bytes, Some(bytes));
            if !in_use && !matches!(record.cache_type.as_str(), "internal" | "frontend") {
                prunable = prunable.saturating_add(bytes);
            }
        }
    }

    let estimate = Estimate {
        build_cache: prunable,
        dangling_images: dangling,
        bytes: dangling.map(|d| d.saturating_add(prunable)),
    };
    Ok(DockerResult::Listed {
        images: image_group,
        containers: container_group,
        volumes: volume_group,
        build_cache: cache_group,
        estimate,
    })
}

/// A sum that stays unknown once a part is.
fn add(sum: Option<u64>, part: Option<u64>) -> Option<u64> {
    Some(sum?.saturating_add(part?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_docker_prints_are_decimal_bytes() {
        for (text, bytes) in [
            ("0B", 0),
            ("512kB", 512_000),
            ("1.23MB", 1_230_000),
            ("12.3GB", 12_300_000_000),
            ("1.1TB", 1_100_000_000_000),
            ("1.234GB", 1_234_000_000),
            ("123.4MB", 123_400_000),
            ("1000kB", 1_000_000),
            ("1e+03kB", 1_000_000),
            ("2.5PB", 2_500_000_000_000_000),
            ("1EB", 1_000_000_000_000_000_000),
            ("999B", 999),
        ] {
            assert_eq!(size(text), Ok(Some(bytes)), "{text}");
        }
        assert_eq!(size("N/A"), Ok(None));
    }

    #[test]
    fn anything_else_is_not_a_size() {
        for text in [
            "", "B", "kB", "12", "12 MB", "12mb", "12KB", "12KiB", "-1MB", "+1MB", "1.MB", ".5MB", "1e3", "1eMB",
            "1e+kB", "NaNMB", "infGB", "1,5MB", "20EB", "1ZB", "n/a", "0x10B", " 1MB",
        ] {
            assert!(size(text).is_err(), "{text:?} read as a size");
        }
    }

    #[test]
    fn flags_are_true_or_false_only() {
        assert_eq!(flag("true"), Ok(true));
        assert_eq!(flag("false"), Ok(false));
        for text in ["", "True", "1", "yes", "N/A"] {
            assert!(flag(text).is_err(), "{text:?}");
        }
    }
}
