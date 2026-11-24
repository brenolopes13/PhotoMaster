//! Leitura de mídia: decodificação de imagens e acesso ao ffmpeg/ffprobe.

use anyhow::{anyhow, bail, Context, Result};
use image::{DynamicImage, ImageDecoder, ImageReader};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn ffmpeg_bin() -> String {
    std::env::var("PHOTOMASTER_FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

pub fn ffprobe_bin() -> String {
    std::env::var("PHOTOMASTER_FFPROBE").unwrap_or_else(|_| "ffprobe".into())
}

/// Cria um `Command` que não abre janela de console no Windows.
pub fn command(bin: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(bin);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c.stdin(Stdio::null());
    c
}

pub fn ffmpeg_available() -> bool {
    command(&ffmpeg_bin())
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration: Option<f64>,
    pub codec: Option<String>,
    pub bitrate: Option<u64>,
    pub has_audio: bool,
}

pub fn ffprobe(path: &Path) -> Result<Probe> {
    let out = command(&ffprobe_bin())
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .context("ffprobe não encontrado (instale o ffmpeg)")?;
    if !out.status.success() {
        bail!("ffprobe falhou: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let mut p = Probe::default();
    let num = |x: &serde_json::Value| -> Option<f64> {
        x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))
    };
    if let Some(streams) = v["streams"].as_array() {
        for s in streams {
            match s["codec_type"].as_str() {
                Some("video") if p.codec.is_none() => {
                    p.codec = s["codec_name"].as_str().map(str::to_string);
                    let (mut w, mut h) = (
                        s["width"].as_u64().map(|v| v as u32),
                        s["height"].as_u64().map(|v| v as u32),
                    );
                    // Vídeos de celular gravados em pé vêm com rotação em metadados.
                    let rot = s["side_data_list"]
                        .as_array()
                        .and_then(|l| l.iter().find_map(|d| num(&d["rotation"])))
                        .or_else(|| num(&s["tags"]["rotate"]))
                        .unwrap_or(0.0);
                    if (rot.abs() as i64) % 180 == 90 {
                        std::mem::swap(&mut w, &mut h);
                    }
                    p.width = w;
                    p.height = h;
                    if p.duration.is_none() {
                        p.duration = num(&s["duration"]);
                    }
                }
                Some("audio") => p.has_audio = true,
                _ => {}
            }
        }
    }
    if let Some(d) = num(&v["format"]["duration"]) {
        p.duration = Some(d);
    }
    p.bitrate = num(&v["format"]["bit_rate"]).map(|b| b as u64);
    Ok(p)
}

/// Extrai um quadro (reduzido a no máx. 512 px de largura) usando o ffmpeg.
/// Serve para vídeos e para formatos de imagem que não decodificamos
/// nativamente (HEIC, por exemplo).
pub fn ffmpeg_frame(path: &Path, at_secs: f64) -> Result<DynamicImage> {
    let mut c = command(&ffmpeg_bin());
    c.args(["-v", "error", "-nostdin"]);
    if at_secs > 0.0 {
        c.args(["-ss", &format!("{at_secs:.3}")]);
    }
    let out = c
        .arg("-i")
        .arg(path)
        .args([
            "-frames:v", "1", "-vf", "scale='min(512,iw)':-2", "-f", "image2pipe", "-vcodec", "png", "-",
        ])
        .output()
        .context("ffmpeg não encontrado")?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("ffmpeg não conseguiu ler um quadro: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Png).map_err(|e| anyhow!(e))
}

/// Decodifica uma imagem aplicando a orientação EXIF (como a galeria mostra).
pub fn load_image_oriented(path: &Path) -> Result<DynamicImage> {
    let mut dec = ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
    let orientation = dec.orientation()?;
    let mut img = DynamicImage::from_decoder(dec)?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// Data em que a foto foi tirada, segundo o EXIF.
pub fn exif_taken_at(path: &Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    let mut r = std::io::BufReader::new(f);
    let ex = exif::Reader::new().read_from_container(&mut r).ok()?;
    ex.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .map(|f| f.display_value().to_string())
}
