//! Built-in self-updater for native Windows and Android builds via GitHub Releases.

use serde::Deserialize;

pub const APP_VERSION: &str = match option_env!("KINO_APP_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[allow(clippy::cast_lossless)]
const fn parse_build_const(raw: Option<&str>) -> Option<u64> {
    let Some(s) = raw else {
        return None;
    };
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut acc: u64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !b.is_ascii_digit() {
            return None;
        }
        acc = acc.saturating_mul(10).saturating_add((b - b'0') as u64);
        i += 1;
    }
    Some(acc)
}

pub const APP_BUILD: Option<u64> = parse_build_const(option_env!("KINO_APP_BUILD"));

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReleaseManifest {
    pub version: String,
    #[serde(default)]
    pub build: u64,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    UpToDate,
    Checking,
    Available(ReleaseManifest),
    Downloading {
        manifest: ReleaseManifest,
        percent: u8,
    },
    Installing(ReleaseManifest),
    Error {
        manifest: Option<ReleaseManifest>,
        message: String,
    },
}

pub fn is_supported() -> bool {
    cfg!(all(
        not(target_arch = "wasm32"),
        any(target_os = "windows", target_os = "android")
    ))
}

pub fn manifest_url() -> Option<&'static str> {
    if cfg!(target_os = "android") {
        Some("https://github.com/geometrik32/Kino/releases/download/android-latest/version.json")
    } else if cfg!(target_os = "windows") {
        Some("https://github.com/geometrik32/Kino/releases/download/desktop-latest/version.json")
    } else {
        None
    }
}

pub fn is_allowed_download_url(url: &str) -> bool {
    let allowed_prefix = if cfg!(target_os = "android") {
        "https://github.com/geometrik32/Kino/releases/download/android-latest/"
    } else {
        "https://github.com/geometrik32/Kino/releases/download/desktop-latest/"
    };
    url.starts_with(allowed_prefix) && url.len() <= 512 && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

pub fn is_newer(remote: &ReleaseManifest) -> bool {
    let Some(remote_ver) = parse_semver(&remote.version) else {
        return false;
    };
    let Some(local_ver) = parse_semver(APP_VERSION) else {
        return false;
    };
    if let Some(local_build) = APP_BUILD {
        if remote.build > 0 {
            return remote.build > local_build && remote_ver >= local_ver;
        }
    }
    remote_ver > local_ver
}

#[cfg(all(not(target_arch = "wasm32"), any(target_os = "windows", target_os = "android")))]
mod imp {
    use std::path::PathBuf;

    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;

    use super::{is_allowed_download_url, is_newer, manifest_url, ReleaseManifest};

    fn http_client() -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .user_agent(format!("OrelKino/{}", super::APP_VERSION))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| format!("Ошибка сети: {e}"))
    }

    pub async fn check_for_update() -> Result<Option<ReleaseManifest>, String> {
        let Some(url) = manifest_url() else {
            return Ok(None);
        };
        let client = http_client()?;
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|_| "Не удалось связаться с сервером обновлений.".to_owned())?;
        if !resp.status().is_success() {
            return Err(format!("Сервер обновлений вернул ошибку ({}).", resp.status().as_u16()));
        }
        let manifest: ReleaseManifest = resp
            .json()
            .await
            .map_err(|_| "Некорректные данные об обновлении.".to_owned())?;
        if !is_allowed_download_url(&manifest.url) {
            return Err("Недопустимый источник обновления.".to_owned());
        }
        Ok(is_newer(&manifest).then_some(manifest))
    }

    pub async fn download_and_install(
        manifest: ReleaseManifest,
        mut on_progress: impl FnMut(u8),
    ) -> Result<(), String> {
        if !is_allowed_download_url(&manifest.url) {
            return Err("Недопустимая ссылка обновления.".to_owned());
        }
        let expected_sha = manifest.sha256.trim().to_ascii_lowercase();
        if expected_sha.len() != 64 || !expected_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Отсутствует контрольная сумма обновления.".to_owned());
        }

        let target_path = update_target_path(&manifest.version).await?;
        if let Some(parent) = target_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Не удалось подготовить папку для обновления: {e}"))?;
        }
        let part_path = target_path.with_extension("part");
        let _ = tokio::fs::remove_file(&part_path).await;

        let client = http_client()?;
        let mut resp = client
            .get(&manifest.url)
            .send()
            .await
            .map_err(|_| "Не удалось скачать файл обновления.".to_owned())?;
        if !resp.status().is_success() {
            return Err(format!(
                "Не удалось скачать обновление (код {}).",
                resp.status().as_u16()
            ));
        }

        let total = resp.content_length().unwrap_or(0);
        let mut downloaded: u64 = 0;
        let mut last_pct: u8 = 0;
        let mut hasher = Sha256::new();
        let mut file = tokio::fs::File::create(&part_path)
            .await
            .map_err(|e| format!("Не удалось создать файл обновления: {e}"))?;

        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|_| "Загрузка обновления прервалась.".to_owned())?
        {
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("Ошибка записи файла обновления: {e}"))?;
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            if total > 0 {
                let pct = ((downloaded.saturating_mul(100)) / total).min(100) as u8;
                if pct != last_pct {
                    last_pct = pct;
                    on_progress(pct);
                }
            }
        }
        file.flush()
            .await
            .map_err(|e| format!("Ошибка сохранения файла обновления: {e}"))?;
        drop(file);

        let actual_sha = format!("{:x}", hasher.finalize());
        if actual_sha != expected_sha {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err("Файл обновления повреждён (не совпала контрольная сумма).".to_owned());
        }

        let _ = tokio::fs::remove_file(&target_path).await;
        tokio::fs::rename(&part_path, &target_path)
            .await
            .map_err(|e| format!("Не удалось подготовить установщик: {e}"))?;

        launch_installer(&target_path).await
    }

    async fn update_target_path(version: &str) -> Result<PathBuf, String> {
        let safe_ver: String = version
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '.')
            .collect();
        #[cfg(target_os = "windows")]
        {
            let name = if safe_ver.is_empty() {
                "Orel-Kino-Setup-update.exe".to_owned()
            } else {
                format!("Orel-Kino-Setup-{safe_ver}.exe")
            };
            Ok(std::env::temp_dir().join(name))
        }
        #[cfg(target_os = "android")]
        {
            let _ = safe_ver;
            let mut dir = None;
            for _ in 0..10 {
                if let Some(d) = crate::platform::android::cache_dir() {
                    dir = Some(d);
                    break;
                }
                crate::platform::sleep(100).await;
            }
            let base = dir.ok_or_else(|| "Не удалось получить папку кэша Android.".to_owned())?;
            Ok(PathBuf::from(base).join("updates").join("orel-kino-update.apk"))
        }
    }

    async fn launch_installer(path: &std::path::Path) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new(path)
                .args(["/S", "/RELAUNCH=1"])
                .spawn()
                .map_err(|e| format!("Не удалось запустить установщик: {e}"))?;
            crate::platform::sleep(250).await;
            std::process::exit(0);
        }
        #[cfg(target_os = "android")]
        {
            let p = path.to_string_lossy().into_owned();
            crate::platform::android::install_apk(&p);
            Ok(())
        }
    }
}

#[cfg(not(all(not(target_arch = "wasm32"), any(target_os = "windows", target_os = "android"))))]
mod imp {
    use super::ReleaseManifest;

    pub async fn check_for_update() -> Result<Option<ReleaseManifest>, String> {
        Ok(None)
    }

    pub async fn download_and_install(_manifest: ReleaseManifest, _on_progress: impl FnMut(u8)) -> Result<(), String> {
        Err("Обновление поддерживается только в приложении для Windows и Android.".into())
    }
}

pub use imp::{check_for_update, download_and_install};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_compares_versions() {
        assert_eq!(parse_semver("0.1.33"), Some((0, 1, 33)));
        assert_eq!(parse_semver("v1.2.10-beta"), Some((1, 2, 10)));
        assert_eq!(parse_semver("bad"), None);

        let older = ReleaseManifest {
            version: "0.1.9".into(),
            build: 0,
            url: "https://github.com/geometrik32/Kino/releases/download/desktop-latest/Orel-Kino-Setup.exe".into(),
            sha256: "a".repeat(64),
            notes: None,
        };
        let newer = ReleaseManifest {
            version: "0.1.10".into(),
            build: 0,
            ..older.clone()
        };
        assert!(!is_newer(&ReleaseManifest {
            version: APP_VERSION.into(),
            build: 0,
            ..older
        }));
        assert!(is_newer(&newer));
    }

    #[test]
    fn rejects_untrusted_download_urls() {
        if cfg!(target_os = "android") {
            assert!(is_allowed_download_url(
                "https://github.com/geometrik32/Kino/releases/download/android-latest/orel-kino-arm64.apk"
            ));
        } else {
            assert!(is_allowed_download_url(
                "https://github.com/geometrik32/Kino/releases/download/desktop-latest/Orel-Kino-Setup.exe"
            ));
        }
        assert!(!is_allowed_download_url(
            "http://github.com/geometrik32/Kino/releases/download/desktop-latest/a.exe"
        ));
        assert!(!is_allowed_download_url("https://evil.example/a.exe"));
    }
}
