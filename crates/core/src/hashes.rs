//! Hashes de conteúdo (para conferir backup) e hashes perceptuais (para
//! achar fotos quase idênticas).

use base64::Engine;
use image::{imageops::FilterType, DynamicImage};
use md5::Md5;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::io::Read;
use std::path::Path;

/// QuickXorHash, o hash que o OneDrive expõe para todos os arquivos.
/// Um XOR circular de 160 bits em que cada byte entra deslocado 11 bits.
pub struct QuickXor {
    data: [u8; 20],
    len: u64,
    shift: usize,
}

impl Default for QuickXor {
    fn default() -> Self {
        Self::new()
    }
}

impl QuickXor {
    pub fn new() -> Self {
        QuickXor { data: [0; 20], len: 0, shift: 0 }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            let byte = self.shift / 8;
            let s = self.shift % 8;
            self.data[byte] ^= b << s;
            if s > 0 {
                self.data[(byte + 1) % 20] ^= b >> (8 - s);
            }
            self.shift = (self.shift + 11) % 160;
        }
        self.len += bytes.len() as u64;
    }

    pub fn finalize(mut self) -> String {
        for (i, b) in self.len.to_le_bytes().iter().enumerate() {
            self.data[12 + i] ^= b;
        }
        base64::engine::general_purpose::STANDARD.encode(self.data)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ContentHashes {
    pub sha1: String,
    pub md5: String,
    pub sha256: String,
    pub quickxor: String,
}

/// Lê o arquivo uma única vez e calcula todos os hashes que os serviços de
/// nuvem suportados publicam.
pub fn content_hashes(path: &Path) -> std::io::Result<ContentHashes> {
    let mut f = std::fs::File::open(path)?;
    let mut sha1 = Sha1::new();
    let mut md5 = Md5::new();
    let mut sha256 = Sha256::new();
    let mut qx = QuickXor::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha1.update(&buf[..n]);
        md5.update(&buf[..n]);
        sha256.update(&buf[..n]);
        qx.update(&buf[..n]);
    }
    Ok(ContentHashes {
        sha1: hex::encode(sha1.finalize()),
        md5: hex::encode(md5.finalize()),
        sha256: hex::encode(sha256.finalize()),
        quickxor: qx.finalize(),
    })
}

/// dHash de 64 bits: compara cada pixel com o vizinho da direita em 9x8.
pub fn dhash(img: &DynamicImage) -> u64 {
    let g = img.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            h <<= 1;
            if g.get_pixel(x, y)[0] > g.get_pixel(x + 1, y)[0] {
                h |= 1;
            }
        }
    }
    h
}

/// aHash de 64 bits: cada pixel de 8x8 comparado com a média.
pub fn ahash(img: &DynamicImage) -> u64 {
    let g = img.resize_exact(8, 8, FilterType::Triangle).to_luma8();
    let mean = g.pixels().map(|p| p[0] as u32).sum::<u32>() / 64;
    let mut h = 0u64;
    for p in g.pixels() {
        h <<= 1;
        if p[0] as u32 > mean {
            h |= 1;
        }
    }
    h
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Nitidez = variância do Laplaciano em tons de cinza. Valores baixos
/// indicam foto borrada. Espera uma imagem já reduzida (~512 px).
pub fn sharpness(img: &DynamicImage) -> f64 {
    let g = img.to_luma8();
    let (w, h) = g.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }
    let px = |x: u32, y: u32| g.get_pixel(x, y)[0] as f64;
    let mut sum = 0.0;
    let mut sum2 = 0.0;
    let mut n = 0.0;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let l = px(x - 1, y) + px(x + 1, y) + px(x, y - 1) + px(x, y + 1) - 4.0 * px(x, y);
            sum += l;
            sum2 += l * l;
            n += 1.0;
        }
    }
    let mean = sum / n;
    sum2 / n - mean * mean
}

/// Brilho médio (0–255).
pub fn brightness(img: &DynamicImage) -> f64 {
    let g = img.to_luma8();
    let n = g.pixels().len().max(1) as f64;
    g.pixels().map(|p| p[0] as f64).sum::<f64>() / n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quickxor_matches_bitwise_definition() {
        // Implementação ingênua, bit a bit, direto da definição do
        // algoritmo: vetor circular de 160 bits, passo de 11 bits por byte,
        // tamanho (u64 LE) aplicado por XOR nos 8 últimos bytes.
        // ATENÇÃO: ainda falta conferir com um arquivo real do OneDrive.
        fn naive(bytes: &[u8]) -> String {
            let mut bits = [false; 160];
            for (i, b) in bytes.iter().enumerate() {
                for k in 0..8 {
                    if b >> k & 1 == 1 {
                        bits[(i * 11 + k) % 160] ^= true;
                    }
                }
            }
            let mut out = [0u8; 20];
            for (i, bit) in bits.iter().enumerate() {
                if *bit {
                    out[i / 8] |= 1 << (i % 8);
                }
            }
            for (i, b) in (bytes.len() as u64).to_le_bytes().iter().enumerate() {
                out[12 + i] ^= b;
            }
            base64::engine::general_purpose::STANDARD.encode(out)
        }
        let h = |b: &[u8]| {
            let mut q = QuickXor::new();
            q.update(b);
            q.finalize()
        };
        assert_eq!(h(b""), "AAAAAAAAAAAAAAAAAAAAAAAAAAA=");
        assert_eq!(h(b"hello world"), naive(b"hello world"));
        let big: Vec<u8> = (0..100_000u32).map(|i| (i * 31 % 251) as u8).collect();
        assert_eq!(h(&big), naive(&big));
        // Atualizações em pedaços devem dar o mesmo resultado.
        let mut q = QuickXor::new();
        for c in big.chunks(777) {
            q.update(c);
        }
        assert_eq!(q.finalize(), h(&big));
    }
}
