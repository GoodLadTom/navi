//! Text to speech with ElevenLabs: your words played back in a voice from your ElevenLabs account.
//! Used by the Speak page, "Hear in voice" in History and the Speak mode (read aloud, not typed).
//!
//! * The key is saved in the Windows Credential Manager as "elevenlabs" (store::secrets), never in
//!   settings or logs.
//! * Audio comes back as raw 16-bit PCM at 24 kHz (`output_format=pcm_24000`, available on every
//!   ElevenLabs plan), so Navi can play it straight away or wrap it as a WAV for the interface.
//! * Errors are short sentences: a refused key, an empty character allowance, a missing voice.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const API: &str = "https://api.elevenlabs.io/v1";
pub const SAMPLE_RATE: u32 = 24_000;
pub const DEFAULT_MODEL: &str = "eleven_v4_turbo";
/// ElevenLabs bills per character, so longer text is cut to this (see `fit`). The Speak page
/// (ui/js/views/speak.js) says when only the first part is read.
pub const MAX_CHARS: usize = 5_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Voice {
    pub voice_id: String,
    pub name: String,
    pub category: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VoiceModel {
    pub model_id: String,
    pub name: String,
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent("Navi dictation")
        .build()
        .map_err(|e| anyhow!("Navi couldn't set up a connection: {e}"))
}

/// The voices in this account (yours, cloned and the ones you added from the library), by name.
pub async fn voices(key: &str) -> Result<Vec<Voice>> {
    let v: serde_json::Value = get(key, "/voices").await?;
    let mut list: Vec<Voice> = v
        .get("voices")
        .and_then(|l| l.as_array())
        .map(|l| {
            l.iter()
                .filter_map(|x| {
                    let labels = x.get("labels");
                    let describe = |k: &str| labels.and_then(|l| l.get(k)).and_then(|s| s.as_str()).unwrap_or_default().to_string();
                    let description = [describe("accent"), describe("gender"), describe("age"), describe("descriptive"), describe("use_case")]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(", ");
                    Some(Voice {
                        voice_id: x.get("voice_id")?.as_str()?.to_string(),
                        name: x.get("name")?.as_str()?.to_string(),
                        category: x.get("category").and_then(|c| c.as_str()).unwrap_or_default().to_string(),
                        description,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(list)
}

/// The text-to-speech models this account can use, v4 first.
pub async fn models(key: &str) -> Result<Vec<VoiceModel>> {
    let v: serde_json::Value = get(key, "/models").await?;
    let mut list: Vec<VoiceModel> = v
        .as_array()
        .map(|l| {
            l.iter()
                .filter(|m| m.get("can_do_text_to_speech").and_then(|b| b.as_bool()).unwrap_or(true))
                .filter_map(|m| Some(VoiceModel { model_id: m.get("model_id")?.as_str()?.to_string(), name: m.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string() }))
                .collect()
        })
        .unwrap_or_default();
    list.sort_by_key(|m| (!m.model_id.contains("v4"), m.model_id.clone()));
    Ok(list)
}

/// `text` read by `voice_id`, as 24 kHz mono samples (-1..1).
pub async fn speak(key: &str, voice_id: &str, model: &str, text: &str) -> Result<Vec<f32>> {
    let voice_id = voice_id.trim();
    if voice_id.is_empty() {
        bail!("Pick a voice first, in Settings > Voice.");
    }
    let (text, cut) = fit(text);
    if text.is_empty() {
        bail!("There's nothing to read out.");
    }
    if cut {
        log::info!("reading the first {} characters of a longer text", text.chars().count());
    }
    let model = if model.trim().is_empty() { DEFAULT_MODEL } else { model.trim() };
    let url = format!("{API}/text-to-speech/{voice_id}?output_format=pcm_{SAMPLE_RATE}");
    let response = client()?
        .post(url)
        .header("xi-api-key", key.trim())
        .json(&serde_json::json!({ "text": text, "model_id": model }))
        .send()
        .await
        .map_err(|e| anyhow!("Couldn't reach ElevenLabs: {}", short(&e.to_string())))?;
    let status = response.status().as_u16();
    let bytes = response.bytes().await.map_err(|e| anyhow!("ElevenLabs stopped part way: {}", short(&e.to_string())))?;
    if status != 200 {
        bail!("{}", explain(status, &String::from_utf8_lossy(&bytes)));
    }
    Ok(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect())
}

/// `text` trimmed and, past MAX_CHARS, cut at the last word break that fits. True when it was
/// cut, so the caller can say only the first part is read.
pub fn fit(text: &str) -> (String, bool) {
    let text = text.trim();
    let Some((limit, next)) = text.char_indices().nth(MAX_CHARS) else {
        return (text.to_string(), false);
    };
    let end = if next.is_whitespace() {
        limit
    } else {
        // Stopping mid-word sounds like a stumble, so go back to the last space if one is near.
        text[..limit].char_indices().rev().take(200).find(|(_, c)| c.is_whitespace()).map_or(limit, |(i, _)| i)
    };
    (text[..end].trim_end().to_string(), true)
}

/// 24 kHz mono samples as a WAV file in memory, for the interface's audio player.
pub fn wav(samples: &[f32]) -> Result<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::new(&mut cursor, spec)?;
        for s in samples {
            w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
        }
        w.finalize()?;
    }
    Ok(cursor.into_inner())
}

async fn get(key: &str, path: &str) -> Result<serde_json::Value> {
    let response = client()?
        .get(format!("{API}{path}"))
        .header("xi-api-key", key.trim())
        .send()
        .await
        .map_err(|e| anyhow!("Couldn't reach ElevenLabs: {}", short(&e.to_string())))?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if status != 200 {
        bail!("{}", explain(status, &body));
    }
    serde_json::from_str(&body).map_err(|_| anyhow!("ElevenLabs sent back something Navi didn't understand."))
}

/// A short sentence for an ElevenLabs error reply. Never includes the key.
fn explain(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/detail/message").or_else(|| v.pointer("/detail")).and_then(|d| d.as_str()).map(str::to_string)
        })
        .unwrap_or_default();
    let lower = detail.to_lowercase();
    match status {
        401 => "ElevenLabs rejected the API key. Check it in Settings > Voice.".into(),
        402 => "Your ElevenLabs plan has run out of characters for now.".into(),
        404 => "ElevenLabs couldn't find that voice. Pick another in Settings > Voice.".into(),
        429 => "ElevenLabs is busy or you've hit your plan's limit. Try again in a moment.".into(),
        _ if lower.contains("quota") || lower.contains("character") => "Your ElevenLabs plan has run out of characters for now.".into(),
        _ if lower.contains("model") => format!("ElevenLabs can't use that model: {}", short(&detail)),
        _ if !detail.is_empty() => format!("ElevenLabs said: {}", short(&detail)),
        _ => format!("ElevenLabs returned an error (status {status})."),
    }
}

fn short(s: &str) -> String {
    let first = s.lines().next().unwrap_or("").trim();
    if first.chars().count() > 140 { first.chars().take(138).collect::<String>() + "…" } else { first.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_read_well() {
        assert!(explain(401, "{}").contains("rejected the API key"));
        assert!(explain(402, "").contains("run out of characters"));
        assert!(explain(400, r#"{"detail":{"status":"quota_exceeded","message":"This request exceeds your quota"}}"#).contains("run out"));
        assert!(explain(422, r#"{"detail":{"message":"model_id eleven_v9 does not exist"}}"#).contains("can't use that model"));
        assert_eq!(explain(500, "oops"), "ElevenLabs returned an error (status 500).");
        for e in [explain(401, ""), explain(404, ""), explain(429, "")] {
            assert!(!e.contains('\u{2014}'), "{e}");
        }
    }

    #[test]
    fn long_text_is_cut_at_a_word() {
        assert_eq!(fit("  Hello there.  "), ("Hello there.".to_string(), false));
        let exact = "a".repeat(MAX_CHARS);
        assert_eq!(fit(&exact), (exact.clone(), false), "exactly the limit is read in full");

        let words = "word ".repeat(1_500); // 7,500 characters
        let (head, cut) = fit(&words);
        assert!(cut);
        assert!(head.chars().count() <= MAX_CHARS);
        assert!(head.chars().count() > MAX_CHARS - 10, "only the broken word is dropped");
        assert!(head.ends_with("word"), "ends on a whole word: {:?}", &head[head.len() - 8..]);

        let unbroken = "é".repeat(MAX_CHARS + 100);
        let (head, cut) = fit(&unbroken);
        assert!(cut);
        assert_eq!(head.chars().count(), MAX_CHARS, "no space to go back to: cut at the limit");
    }

    #[test]
    fn wav_wraps_the_samples() {
        let samples = vec![0.0f32, 0.5, -0.5, 1.0];
        let bytes = wav(&samples).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(bytes.len(), 44 + samples.len() * 2);
    }

    /// Lists voices and models and reads one short line with the key in ELEVENLABS_API_KEY (never
    /// printed). Run by hand: cargo test --lib speak::tests::live_elevenlabs -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_elevenlabs() {
        let key = std::env::var("ELEVENLABS_API_KEY").expect("ELEVENLABS_API_KEY not set");
        let voices = voices(&key).await.expect("voices");
        let models = models(&key).await.expect("models");
        println!("{} voices; models: {:?}", voices.len(), models.iter().map(|m| m.model_id.as_str()).collect::<Vec<_>>());
        let voice = voices.first().expect("a voice");
        let model = models.iter().find(|m| m.model_id == DEFAULT_MODEL).map(|m| m.model_id.clone()).unwrap_or_else(|| models[0].model_id.clone());
        let t = std::time::Instant::now();
        let samples = speak(&key, &voice.voice_id, &model, "Hi, this is Navi.").await.expect("speech");
        println!("{} read \"Hi, this is Navi.\" with {model}: {:.2} s of audio in {} ms", voice.name, samples.len() as f32 / SAMPLE_RATE as f32, t.elapsed().as_millis());
        assert!(samples.len() > SAMPLE_RATE as usize / 2);
    }

    #[tokio::test]
    async fn an_empty_voice_or_text_says_so() {
        assert!(speak("k", "", DEFAULT_MODEL, "hi").await.unwrap_err().to_string().contains("Pick a voice"));
        assert!(speak("k", "abc", DEFAULT_MODEL, "   ").await.unwrap_err().to_string().contains("nothing to read"));
    }
}
