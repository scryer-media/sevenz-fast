use std::io::Read;
use std::sync::Arc;

#[cfg(feature = "bzip2")]
use bzip2::read::BzDecoder;
#[cfg(feature = "deflate")]
use flate2::bufread::DeflateDecoder;
use lzma_fast::LzmaReader;
#[cfg(feature = "ppmd")]
use ppmd_rust::{
    PPMD7_MAX_MEM_SIZE, PPMD7_MAX_ORDER, PPMD7_MIN_MEM_SIZE, PPMD7_MIN_ORDER, Ppmd7Decoder,
};

#[cfg(feature = "brotli")]
use crate::codec::brotli::BrotliDecoder;
#[cfg(feature = "lz4")]
use crate::codec::lz4::Lz4Decoder;
use crate::codec::{
    filter::{bcj::BcjReader, delta::DeltaReader},
    lzma_fast::{
        Lzma2Coder, Lzma2Control, Lzma2Plan, lzma2_clamped_prop, lzma2_decoder,
        lzma2_dictionary_size, lzma2_memory_usage_kb,
    },
};
use crate::container::ArchiveLimits;
#[cfg(feature = "aes256")]
use crate::encryption::Aes256Sha256Decoder;
use crate::{Password, archive::EncoderMethod, block::Coder, error::Error};

/// Everything a coder needs that is not in the archive: the caller's limits,
/// how many threads they will allow, and the live link to the LZMA2 coder.
///
/// It replaces the two loose parameters (`max_mem_limit_kb`, `threads`) that
/// upstream threads through the decode-stack builders. Bundling them is what
/// lets the LZMA2 coder be handed a control block as well without every
/// function between here and the reader growing a ninth argument.
pub(crate) struct DecodeOptions<'a> {
    /// What the caller will let the archive allocate.
    pub(crate) limits: &'a ArchiveLimits,
    /// Thread ceiling for coders that can use one. One means inline.
    pub(crate) threads: u32,
    /// Build the LZMA2 coder so it can widen later even at one thread.
    pub(crate) adaptive_lzma2: bool,
    /// Whether the header's checksums are to be checked at all. False only
    /// when the caller has said it verifies the bytes by other means.
    pub(crate) verify_checksums: bool,
    /// The live link back to the reader, when there is one. The header decode
    /// has none: it is one small block on the calling thread, before the
    /// caller has had any chance to ask for anything else.
    pub(crate) lzma2_control: Option<&'a Arc<Lzma2Control>>,
    /// Where the consumer's boundaries fall in this block's decoded stream —
    /// the offsets its files start at — so that a parallel LZMA2 coder can
    /// checksum each piece in the worker that produced it. Empty when nothing
    /// is to be checksummed there.
    pub(crate) checksum_splits: &'a [u64],
}

impl<'a> DecodeOptions<'a> {
    /// The options the header decode runs with: the caller's limits, one
    /// thread, no live control.
    pub(crate) fn header(limits: &'a ArchiveLimits) -> Self {
        Self {
            limits,
            threads: 1,
            adaptive_lzma2: false,
            verify_checksums: true,
            lzma2_control: None,
            checksum_splits: &[],
        }
    }

    /// Whether the checksums of this block are being computed by the LZMA2
    /// workers rather than by whoever consumes the output.
    ///
    /// True only once the coder has actually been built and engaged the
    /// parallel path: a plan can always degrade to the single-threaded
    /// decoder, which computes no checksums, so the answer is read from the
    /// live coder and not from what was asked for.
    pub(crate) fn folding_checksums(&self) -> bool {
        !self.checksum_splits.is_empty()
            && self
                .lzma2_control
                .is_some_and(|control| control.progress().is_some())
    }
}

pub enum Decoder<R: Read> {
    Copy(R),
    Lzma(Box<LzmaReader<R>>),
    Lzma2(Box<Lzma2Coder<R>>),
    #[cfg(feature = "ppmd")]
    Ppmd(Box<Ppmd7Decoder<R>>),
    Bcj(BcjReader<R>),
    Delta(DeltaReader<R>),
    #[cfg(feature = "brotli")]
    Brotli(Box<BrotliDecoder<R>>),
    #[cfg(feature = "bzip2")]
    Bzip2(BzDecoder<R>),
    #[cfg(feature = "deflate")]
    Deflate(DeflateDecoder<std::io::BufReader<R>>),
    #[cfg(feature = "lz4")]
    Lz4(Lz4Decoder<R>),
    #[cfg(feature = "zstd")]
    Zstd(zstd::Decoder<'static, std::io::BufReader<R>>),
    #[cfg(feature = "aes256")]
    Aes256Sha256(Box<Aes256Sha256Decoder<R>>),
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Decoder::Copy(r) => r.read(buf),
            Decoder::Lzma(r) => r.read(buf),
            Decoder::Lzma2(r) => r.read(buf),
            #[cfg(feature = "ppmd")]
            Decoder::Ppmd(r) => r.read(buf),
            Decoder::Bcj(r) => r.read(buf),
            Decoder::Delta(r) => r.read(buf),
            #[cfg(feature = "brotli")]
            Decoder::Brotli(r) => r.read(buf),
            #[cfg(feature = "bzip2")]
            Decoder::Bzip2(r) => r.read(buf),
            #[cfg(feature = "deflate")]
            Decoder::Deflate(r) => r.read(buf),
            #[cfg(feature = "lz4")]
            Decoder::Lz4(r) => r.read(buf),
            #[cfg(feature = "zstd")]
            Decoder::Zstd(r) => r.read(buf),
            #[cfg(feature = "aes256")]
            Decoder::Aes256Sha256(r) => r.read(buf),
        }
    }
}

pub fn add_decoder<I: Read>(
    input: I,
    uncompressed_len: usize,
    coder: &Coder,
    #[allow(unused)] password: &Password,
    opts: &DecodeOptions<'_>,
) -> Result<Decoder<I>, Error> {
    let max_mem_limit_kb = opts.limits.memory_limit_kb();
    let method = EncoderMethod::by_id(coder.encoder_method_id());
    let method = if let Some(m) = method {
        m
    } else {
        return Err(Error::UnsupportedCompressionMethod(format!(
            "{:?}",
            coder.encoder_method_id()
        )));
    };
    match method.id() {
        EncoderMethod::ID_COPY => Ok(Decoder::Copy(input)),
        EncoderMethod::ID_LZMA => {
            // Validate the length before touching the properties: the decoder
            // slices `[1..5]`, which would panic on an attacker-supplied short field.
            if coder.properties.len() < 5 {
                return Err(Error::Other("LZMA properties too short".into()));
            }
            // Clamp before the budget check, so a coder that declares a huge
            // dictionary for a small stream is decoded rather than refused.
            let dict_size = crate::codec::lzma_fast::clamp_dictionary(
                crate::codec::lzma_fast::lzma_dictionary_size(&coder.properties)?,
                uncompressed_len as u64,
            );
            let mem_size = lzma2_memory_usage_kb(dict_size);
            if mem_size > max_mem_limit_kb {
                return Err(Error::MaxMemLimited {
                    max_kb: max_mem_limit_kb,
                    actaul_kb: mem_size,
                });
            }
            let lz = crate::codec::lzma_fast::lzma_decoder(
                input,
                uncompressed_len,
                &coder.properties,
                dict_size,
            )
            .map_err(|e| Error::bad_password(e, !password.is_empty()))?;
            Ok(Decoder::Lzma(Box::new(lz)))
        }
        EncoderMethod::ID_LZMA2 => {
            // The dictionary is a table index rather than a number here, so the
            // clamp comes back as the property byte the decoders are built from.
            let dict_prop = coder
                .properties
                .first()
                .copied()
                .ok_or_else(|| Error::other("LZMA2 properties too short"))?;
            let dict_prop = lzma2_clamped_prop(dict_prop, uncompressed_len as u64);
            let dic_size = lzma2_dictionary_size(&[dict_prop])?;
            let mem_size = lzma2_memory_usage_kb(dic_size);
            if mem_size > max_mem_limit_kb {
                return Err(Error::MaxMemLimited {
                    max_kb: max_mem_limit_kb,
                    actaul_kb: mem_size,
                });
            }

            let plan = match opts.lzma2_control {
                Some(control) => Lzma2Plan::for_block(
                    opts.threads,
                    opts.adaptive_lzma2,
                    opts.limits.memory_limit_bytes,
                    dic_size,
                    control,
                    opts.checksum_splits,
                ),
                None => Lzma2Plan::SingleThreaded,
            };
            let lz = lzma2_decoder(input, dict_prop, plan)
                .map_err(|e| Error::bad_password(e, !password.is_empty()))?;
            Ok(Decoder::Lzma2(Box::new(lz)))
        }
        #[cfg(feature = "ppmd")]
        EncoderMethod::ID_PPMD => {
            let (order, memory_size) = get_ppmd_order_memory_size(coder, max_mem_limit_kb)?;
            let ppmd = Ppmd7Decoder::new(input, order, memory_size)
                .map_err(|err| Error::other(err.to_string()))?;
            Ok(Decoder::Ppmd(Box::new(ppmd)))
        }
        #[cfg(feature = "brotli")]
        EncoderMethod::ID_BROTLI => {
            let de = BrotliDecoder::new(input, 4096)?;
            Ok(Decoder::Brotli(Box::new(de)))
        }
        #[cfg(feature = "bzip2")]
        EncoderMethod::ID_BZIP2 => {
            let de = BzDecoder::new(input);
            Ok(Decoder::Bzip2(de))
        }
        #[cfg(feature = "deflate")]
        EncoderMethod::ID_DEFLATE => {
            let buf_read = std::io::BufReader::new(input);
            let de = DeflateDecoder::new(buf_read);
            Ok(Decoder::Deflate(de))
        }
        #[cfg(feature = "lz4")]
        EncoderMethod::ID_LZ4 => {
            let de = Lz4Decoder::new(input)?;
            Ok(Decoder::Lz4(de))
        }
        #[cfg(feature = "zstd")]
        EncoderMethod::ID_ZSTD => {
            let mut zs = zstd::Decoder::new(input)?;
            // A zstd frame declares its own back-reference window, and the
            // decoder allocates it. The format allows windows far larger than
            // any 7z encoder writes, so the window is bounded here: by the
            // caller's memory limit when there is one, and otherwise by the
            // 128 MiB the reference decoder itself refuses to exceed.
            const ZSTD_DEFAULT_WINDOW_LOG: u32 = 27;
            let window_log = if max_mem_limit_kb == usize::MAX {
                ZSTD_DEFAULT_WINDOW_LOG
            } else {
                let bytes = (max_mem_limit_kb as u64).saturating_mul(1024).max(1024);
                // The largest power of two that fits in the budget, never above
                // the default and never below the 1 KiB floor the format has.
                (63 - bytes.leading_zeros()).clamp(10, ZSTD_DEFAULT_WINDOW_LOG)
            };
            zs.window_log_max(window_log)?;
            Ok(Decoder::Zstd(zs))
        }
        EncoderMethod::ID_BCJ_X86 => {
            let de = BcjReader::new_x86(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_ARM => {
            let de = BcjReader::new_arm(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_ARM64 => {
            let de = BcjReader::new_arm64(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_ARM_THUMB => {
            let de = BcjReader::new_arm_thumb(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_PPC => {
            let de = BcjReader::new_ppc(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_IA64 => {
            let de = BcjReader::new_ia64(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_SPARC => {
            let de = BcjReader::new_sparc(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_RISCV => {
            let de = BcjReader::new_riscv(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_DELTA => {
            // The distance is `properties[0] + 1` in the range 1..=256. Widen to `usize`
            // before the `+1` so a property byte of `0xFF` yields 256, not 0 (a `u8`
            // `wrapping_add` would wrap to a zero distance and mis-decode / divide by zero).
            let d = coder.properties.first().map_or(1, |b| *b as usize + 1);
            let de = DeltaReader::new(input, d);
            Ok(Decoder::Delta(de))
        }
        #[cfg(feature = "aes256")]
        EncoderMethod::ID_AES256_SHA256 => {
            if password.is_empty() {
                return Err(Error::PasswordRequired);
            }
            let de = Aes256Sha256Decoder::new(
                input,
                &coder.properties,
                password,
                opts.limits.max_aes_cycles_power,
            )?;
            Ok(Decoder::Aes256Sha256(Box::new(de)))
        }
        _ => Err(Error::UnsupportedCompressionMethod(
            method.name().to_string(),
        )),
    }
}

#[cfg(feature = "ppmd")]
fn get_ppmd_order_memory_size(coder: &Coder, max_mem_limit_kb: usize) -> Result<(u32, u32), Error> {
    if coder.properties.len() < 5 {
        return Err(Error::other("PPMD properties too short"));
    }
    let order = coder.properties[0] as u32;
    let memory_size = u32::from_le_bytes([
        coder.properties[1],
        coder.properties[2],
        coder.properties[3],
        coder.properties[4],
    ]);

    if order < PPMD7_MIN_ORDER {
        return Err(Error::other("PPMD order smaller than PPMD7_MIN_ORDER"));
    }

    if order > PPMD7_MAX_ORDER {
        return Err(Error::other("PPMD order larger than PPMD7_MAX_ORDER"));
    }

    if memory_size < PPMD7_MIN_MEM_SIZE {
        return Err(Error::other(
            "PPMD memory size smaller than PPMD7_MIN_MEM_SIZE",
        ));
    }

    if memory_size > PPMD7_MAX_MEM_SIZE {
        return Err(Error::other(
            "PPMD memory size larger than PPMD7_MAX_MEM_SIZE",
        ));
    }

    let memory_size_kb = memory_size.div_ceil(1024) as usize;
    if memory_size_kb > max_mem_limit_kb {
        return Err(Error::MaxMemLimited {
            max_kb: max_mem_limit_kb,
            actaul_kb: memory_size_kb,
        });
    }

    Ok((order, memory_size))
}
