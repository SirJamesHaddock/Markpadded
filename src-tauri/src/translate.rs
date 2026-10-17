//! Translating a selection in the preview.
//!
//! - `detect_language` guesses what language a selection is in, locally.
//! - `translate_google` sends it to Google Translate's free web endpoint (the
//!   one translate.google.com's widget uses; unofficial, no key).
//! - The `offline_translation_*` commands fetch what Firefox's offline engine
//!   runs on — the Bergamot WASM and Mozilla's per-language-pair models — from
//!   the same Remote Settings service Firefox uses, and cache them on disk. The
//!   engine itself runs in a worker in the webview (`static/translate/`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Manager;
use whatlang::{Detector, Lang};

use crate::commands::blocking;

const GOOGLE_ENDPOINT: &str = "https://translate.googleapis.com/translate_a/single";
const GOOGLE_TIMEOUT: Duration = Duration::from_secs(10);
/// Google's web endpoint answers an error past roughly this many characters.
const MAX_GOOGLE_CHARS: usize = 5000;

const REMOTE_SETTINGS: &str =
    "https://firefox.settings.services.mozilla.com/v1/buckets/main/collections";
const ATTACHMENTS: &str = "https://firefox-settings-attachments.cdn.mozilla.net";
const MODELS_COLLECTION: &str = "translations-models-v2";
const WASM_COLLECTION: &str = "translations-wasm-v2";
/// `static/translate/bergamot-translator.js` is the emscripten glue for this
/// major version of the WASM; a newer major would not load with it.
const WASM_MAJOR: u32 = 4;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// The largest compressed model Mozilla publishes is about 30 MB.
const MAX_DOWNLOAD_BYTES: usize = 150 * 1024 * 1024;
/// Model and vocabulary files a pair may have; the worker aligns each kind.
const MODEL_FILE_TYPES: [&str; 5] = ["model", "lex", "vocab", "srcvocab", "trgvocab"];

/// The languages detection chooses between, and the code the frontend uses
/// for each. Restricting the list is what makes short selections usable:
/// whatlang no longer has to rule out seventy languages nobody reads here.
const DETECTABLE: [(Lang, &str); 12] = [
    (Lang::Eng, "en"),
    (Lang::Ita, "it"),
    (Lang::Deu, "de"),
    (Lang::Spa, "es"),
    (Lang::Swe, "sv"),
    (Lang::Fra, "fr"),
    (Lang::Dan, "da"),
    (Lang::Nob, "no"),
    (Lang::Fin, "fi"),
    (Lang::Rus, "ru"),
    (Lang::Pol, "pl"),
    (Lang::Por, "pt"),
];

/// Below this, a guess is shown only when whatlang itself calls it reliable.
const MIN_CONFIDENCE: f64 = 0.5;

#[derive(Debug, Serialize)]
pub struct DetectedLanguage {
    code: &'static str,
    /// Whether the guess is good enough to show before the user asks.
    confident: bool,
}

fn detect(text: &str) -> Option<DetectedLanguage> {
    let detector = Detector::with_allowlist(DETECTABLE.iter().map(|(lang, _)| *lang).collect());
    let info = detector.detect(text)?;
    let code = DETECTABLE.iter().find(|(lang, _)| *lang == info.lang())?.1;
    Some(DetectedLanguage {
        code,
        confident: info.is_reliable() || info.confidence() >= MIN_CONFIDENCE,
    })
}

#[tauri::command]
pub fn detect_language(text: String) -> Result<Option<DetectedLanguage>, String> {
    Ok(detect(&text))
}

#[derive(Debug, Serialize, PartialEq)]
pub struct GoogleTranslation {
    text: String,
    /// The source language Google worked from, which is its own detection
    /// when `from` was `auto`.
    detected: Option<String>,
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// The endpoint answers a nested array: `[[[translated, source, …], …], null, "de", …]`.
fn parse_google(body: &str) -> Result<GoogleTranslation, String> {
    let shape = || "Google's reply was not in the shape Markpad expects".to_string();
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| shape())?;
    let segments = value.get(0).and_then(|v| v.as_array()).ok_or_else(shape)?;
    let text: String = segments
        .iter()
        .filter_map(|segment| segment.get(0).and_then(|v| v.as_str()))
        .collect();
    if text.is_empty() {
        return Err(shape());
    }
    let detected = value.get(2).and_then(|v| v.as_str()).map(str::to_string);
    Ok(GoogleTranslation { text, detected })
}

#[tauri::command]
pub async fn translate_google(
    text: String,
    from: String,
    to: String,
) -> Result<GoogleTranslation, String> {
    if text.chars().count() > MAX_GOOGLE_CHARS {
        return Err(format!(
            "The selection is too long for Google ({MAX_GOOGLE_CHARS} characters at most)"
        ));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(GOOGLE_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    // POST, so a long selection does not run into URL length limits.
    let url = format!(
        "{GOOGLE_ENDPOINT}?client=gtx&dt=t&sl={}&tl={}",
        encode(&from),
        encode(&to)
    );
    let response = client
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded;charset=UTF-8",
        )
        .body(format!("q={}", encode(&text)))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("Google answered HTTP {}", response.status()));
    }
    let body = response.text().await.map_err(|e| e.to_string())?;
    parse_google(&body)
}

#[derive(Debug, Clone, Deserialize)]
struct Attachment {
    location: String,
}

/// One Remote Settings record: a model file, or the WASM.
#[derive(Debug, Clone, Deserialize)]
struct Record {
    name: String,
    version: String,
    #[serde(rename = "fileType", default)]
    file_type: Option<String>,
    #[serde(default)]
    filter_expression: String,
    #[serde(rename = "decompressedHash", default)]
    decompressed_hash: Option<String>,
    attachment: Attachment,
}

#[derive(Deserialize)]
struct Records {
    data: Vec<Record>,
}

/// `"3.1"` → `(3, 1)`. Pre-releases such as `"3.0a1"` are not released.
fn release_version(version: &str) -> Option<(u32, u32)> {
    let (major, minor) = version.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Records Firefox on the desktop release channel would use. Others are
/// Android-only, or nightly-only models still being evaluated.
fn applies_to_desktop(record: &Record) -> bool {
    let filter = record.filter_expression.trim();
    filter.is_empty() || filter == "env.appinfo.OS != 'Android'"
}

/// The newest released version of a pair that has a complete set of files:
/// a model, and a vocabulary (shared, or one per side). The shortlist (`lex`)
/// is optional. Returns one record per file type.
fn pick_model_files(records: &[Record]) -> Option<Vec<Record>> {
    let mut versions: Vec<(u32, u32)> = records
        .iter()
        .filter(|r| applies_to_desktop(r))
        .filter_map(|r| release_version(&r.version))
        .collect();
    versions.sort_unstable();
    versions.dedup();

    versions.into_iter().rev().find_map(|version| {
        let mut files: Vec<Record> = Vec::new();
        for record in records {
            let Some(kind) = record.file_type.as_deref() else {
                continue;
            };
            if !applies_to_desktop(record)
                || release_version(&record.version) != Some(version)
                || !MODEL_FILE_TYPES.contains(&kind)
                || files.iter().any(|f| f.file_type.as_deref() == Some(kind))
            {
                continue;
            }
            files.push(record.clone());
        }
        let has = |kind: &str| files.iter().any(|f| f.file_type.as_deref() == Some(kind));
        let complete = has("model") && (has("vocab") || (has("srcvocab") && has("trgvocab")));
        complete.then_some(files)
    })
}

/// The newest WASM of the major version the vendored glue was built for.
fn pick_wasm(records: &[Record]) -> Option<Record> {
    records
        .iter()
        .filter(|r| r.name == "bergamot-translator" && applies_to_desktop(r))
        .filter_map(|r| release_version(&r.version).map(|v| (v, r)))
        .filter(|((major, _), _)| *major == WASM_MAJOR)
        .max_by_key(|(version, _)| *version)
        .map(|(_, r)| r.clone())
}

/// Language codes become directory names, so they are held to what a
/// language code looks like.
fn valid_language(code: &str) -> bool {
    (2..=3).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase())
}

fn translations_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("translations"))
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch_records(
    client: &reqwest::Client,
    collection: &str,
    query: &str,
) -> Result<Vec<Record>, String> {
    let url = format!("{REMOTE_SETTINGS}/{collection}/records{query}");
    let response = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Mozilla's model list answered HTTP {}",
            response.status()
        ));
    }
    let body = response.text().await.map_err(|e| e.to_string())?;
    serde_json::from_str::<Records>(&body)
        .map(|records| records.data)
        .map_err(|e| e.to_string())
}

/// Downloads a record's attachment, decompresses it and checks it against the
/// hash Mozilla publishes.
async fn fetch_attachment(client: &reqwest::Client, record: &Record) -> Result<Vec<u8>, String> {
    let url = format!("{ATTACHMENTS}/{}", record.attachment.location);
    let mut response = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Downloading {} answered HTTP {}",
            record.name,
            response.status()
        ));
    }
    let mut compressed = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if compressed.len() + chunk.len() > MAX_DOWNLOAD_BYTES {
            return Err(format!("{} is larger than expected", record.name));
        }
        compressed.extend_from_slice(&chunk);
    }
    let zstd = record.attachment.location.ends_with(".zst");
    let expected = record.decompressed_hash.clone();
    let name = record.name.clone();
    blocking(move || {
        let bytes = if zstd {
            decompress(&compressed)?
        } else {
            compressed
        };
        if let Some(expected) = expected {
            let actual = format!("{:x}", Sha256::digest(&bytes));
            if !actual.eq_ignore_ascii_case(&expected) {
                return Err(format!("{name} did not match its published checksum"));
            }
        }
        Ok(bytes)
    })
    .await
}

fn decompress(compressed: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder =
        ruzstd::decoding::StreamingDecoder::new(compressed).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    decoder.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Writes through a temporary file so a reader never sees half a model.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, path).map_err(|e| e.to_string())
}

/// What the worker needs to build a model for one direct language pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfflineModel {
    source: String,
    target: String,
    version: String,
    /// The file types on disk, out of `MODEL_FILE_TYPES`.
    files: Vec<String>,
    /// The model file's own name, which says how its weights are quantised.
    model_name: String,
}

fn pair_dir(root: &Path, source: &str, target: &str) -> PathBuf {
    root.join("models").join(format!("{source}-{target}"))
}

fn read_manifest(dir: &Path) -> Option<OfflineModel> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
    let manifest: OfflineModel = serde_json::from_str(&text).ok()?;
    manifest
        .files
        .iter()
        .all(|kind| dir.join(format!("{kind}.bin")).is_file())
        .then_some(manifest)
}

/// Makes sure the model files for `source` → `target` are on disk, downloading
/// them from Mozilla the first time. Only direct pairs: the frontend pivots
/// through English for two non-English languages.
#[tauri::command]
pub async fn offline_translation_prepare(
    app: tauri::AppHandle,
    source: String,
    target: String,
) -> Result<OfflineModel, String> {
    if !valid_language(&source) || !valid_language(&target) {
        return Err(format!("Not a language code: {source} → {target}"));
    }
    let dir = pair_dir(&translations_dir(&app)?, &source, &target);
    if let Some(manifest) = read_manifest(&dir) {
        return Ok(manifest);
    }

    let client = client()?;
    let records = fetch_records(
        &client,
        MODELS_COLLECTION,
        &format!("?sourceLanguage={source}&targetLanguage={target}"),
    )
    .await?;
    let files = pick_model_files(&records)
        .ok_or_else(|| format!("Firefox has no offline model for {source} → {target}"))?;

    for record in &files {
        let bytes = fetch_attachment(&client, record).await?;
        let path = dir.join(format!(
            "{}.bin",
            record.file_type.as_deref().unwrap_or_default()
        ));
        blocking(move || write_file(&path, &bytes)).await?;
    }

    let manifest = OfflineModel {
        source,
        target,
        version: files[0].version.clone(),
        files: files.iter().filter_map(|f| f.file_type.clone()).collect(),
        model_name: files
            .iter()
            .find(|f| f.file_type.as_deref() == Some("model"))
            .map(|f| f.name.clone())
            .unwrap_or_default(),
    };
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let manifest_path = dir.join("manifest.json");
    blocking(move || write_file(&manifest_path, &json)).await?;
    Ok(manifest)
}

/// One prepared model file, as an `ArrayBuffer` on the JS side.
#[tauri::command]
pub async fn offline_translation_file(
    app: tauri::AppHandle,
    source: String,
    target: String,
    file_type: String,
) -> Result<tauri::ipc::Response, String> {
    if !valid_language(&source)
        || !valid_language(&target)
        || !MODEL_FILE_TYPES.contains(&file_type.as_str())
    {
        return Err(format!("Not a model file: {source} → {target} {file_type}"));
    }
    let path =
        pair_dir(&translations_dir(&app)?, &source, &target).join(format!("{file_type}.bin"));
    let bytes = blocking(move || std::fs::read(&path).map_err(|e| e.to_string())).await?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// The Bergamot WASM, downloaded the first time and cached after that.
#[tauri::command]
pub async fn offline_translation_wasm(
    app: tauri::AppHandle,
) -> Result<tauri::ipc::Response, String> {
    let dir = translations_dir(&app)?.join("wasm");
    let cached = dir.join(format!("bergamot-{WASM_MAJOR}.wasm"));
    let cached_read = cached.clone();
    if let Ok(bytes) =
        blocking(move || std::fs::read(&cached_read).map_err(|e| e.to_string())).await
    {
        return Ok(tauri::ipc::Response::new(bytes));
    }

    let client = client()?;
    let records = fetch_records(&client, WASM_COLLECTION, "").await?;
    let record = pick_wasm(&records).ok_or_else(|| {
        format!("Mozilla no longer publishes version {WASM_MAJOR} of the offline engine")
    })?;
    let bytes = fetch_attachment(&client, &record).await?;
    let to_write = bytes.clone();
    blocking(move || write_file(&cached, &to_write)).await?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, version: &str, file_type: &str, filter: &str) -> Record {
        Record {
            name: name.to_string(),
            version: version.to_string(),
            file_type: Some(file_type.to_string()),
            filter_expression: filter.to_string(),
            decompressed_hash: None,
            attachment: Attachment {
                location: format!("{name}-{version}.zst"),
            },
        }
    }

    #[test]
    fn parses_a_multi_sentence_google_reply() {
        // Recorded from the endpoint, trimmed of the per-sentence metadata.
        let body = r#"[[["If a file has been changed, Markpad will ask. ","Wenn eine Datei geändert wurde, fragt Markpad. ",null,null,3],["Second sentence.","Zweiter Satz.",null,null,3]],null,"de",null,null,null,1,[],[["de"],null,[1],["de"]]]"#;
        assert_eq!(
            parse_google(body).unwrap(),
            GoogleTranslation {
                text: "If a file has been changed, Markpad will ask. Second sentence.".to_string(),
                detected: Some("de".to_string()),
            }
        );
    }

    #[test]
    fn rejects_a_reply_in_another_shape() {
        assert!(parse_google("<html>429 Too Many Requests</html>").is_err());
        assert!(parse_google(r#"{"error":"x"}"#).is_err());
        assert!(parse_google("[[],null,\"de\"]").is_err());
    }

    #[test]
    fn pre_releases_are_not_versions() {
        assert_eq!(release_version("3.1"), Some((3, 1)));
        assert_eq!(release_version("3.0a1"), None);
        assert_eq!(release_version("3"), None);
    }

    #[test]
    fn picks_the_newest_complete_desktop_release() {
        let records = vec![
            record("model.ruen.v30.bin", "3.0", "model", ""),
            record("vocab.ruen.v30.spm", "3.0", "vocab", ""),
            record("lex.ruen.v30.bin", "3.0", "lex", ""),
            record("model.ruen.v31.bin", "3.1", "model", ""),
            record("vocab.ruen.v31.spm", "3.1", "vocab", ""),
            // Newer, but only for Nightly or only for Android.
            record(
                "model.ruen.v40.bin",
                "4.0",
                "model",
                "env.channel == 'default' || env.channel == 'nightly'",
            ),
            record(
                "vocab.ruen.v40.spm",
                "4.0",
                "vocab",
                "env.channel == 'default' || env.channel == 'nightly'",
            ),
            record(
                "model.ruen.v32.bin",
                "3.2",
                "model",
                "env.appinfo.OS == 'Android'",
            ),
            record(
                "vocab.ruen.v32.spm",
                "3.2",
                "vocab",
                "env.appinfo.OS == 'Android'",
            ),
            record("model.ruen.v39a1.bin", "3.9a1", "model", ""),
            record("vocab.ruen.v39a1.spm", "3.9a1", "vocab", ""),
        ];
        let picked = pick_model_files(&records).unwrap();
        let names: Vec<&str> = picked.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["model.ruen.v31.bin", "vocab.ruen.v31.spm"]);
    }

    #[test]
    fn a_version_without_a_vocabulary_is_skipped() {
        let records = vec![
            record("model.v31.bin", "3.1", "model", ""),
            record("model.v30.bin", "3.0", "model", ""),
            record("src.v30.spm", "3.0", "srcvocab", ""),
            record("trg.v30.spm", "3.0", "trgvocab", ""),
        ];
        let picked = pick_model_files(&records).unwrap();
        assert!(picked.iter().all(|r| r.version == "3.0"));
        assert_eq!(picked.len(), 3);
    }

    #[test]
    fn the_wasm_stays_on_the_glue_s_major_version() {
        let records = vec![
            record("bergamot-translator", "3.9", "", ""),
            record("bergamot-translator", "4.0", "", ""),
            record("bergamot-translator", "4.2", "", ""),
            record("bergamot-translator", "5.0", "", ""),
        ];
        assert_eq!(pick_wasm(&records).unwrap().version, "4.2");
    }

    #[test]
    fn language_codes_cannot_leave_the_models_directory() {
        assert!(valid_language("de"));
        assert!(valid_language("nb"));
        assert!(!valid_language("../x"));
        assert!(!valid_language("DE"));
        assert!(!valid_language("pt-PT"));
    }

    #[test]
    fn detects_each_language_from_a_sentence() {
        let samples = [
            (
                "en",
                "The settings are stored in a file in your home folder.",
            ),
            (
                "it",
                "Le impostazioni sono salvate in un file nella cartella dell'utente.",
            ),
            (
                "de",
                "Die Einstellungen werden in einer Datei im Benutzerordner gespeichert.",
            ),
            (
                "es",
                "La configuración se guarda en un archivo en la carpeta del usuario.",
            ),
            ("sv", "Inställningarna sparas i en fil i användarens mapp."),
            (
                "fr",
                "Les paramètres sont enregistrés dans un fichier du dossier utilisateur.",
            ),
            (
                "da",
                "Indstillingerne gemmes i en fil i brugerens mappe, og de bliver ikke slettet.",
            ),
            (
                "no",
                "Innstillingene lagres i en fil i brukerens mappe, og de blir ikke slettet.",
            ),
            (
                "fi",
                "Asetukset tallennetaan tiedostoon käyttäjän kansiossa.",
            ),
            ("ru", "Настройки хранятся в файле в папке пользователя."),
            (
                "pl",
                "Ustawienia są zapisywane w pliku w folderze użytkownika.",
            ),
            (
                "pt",
                "As configurações são salvas em um arquivo na pasta do usuário.",
            ),
        ];
        for (code, text) in samples {
            let detected = detect(text).unwrap_or_else(|| panic!("nothing detected for {code}"));
            assert_eq!(detected.code, code, "{text}");
        }
    }
}
