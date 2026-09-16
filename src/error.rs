use std::{borrow::Cow, fmt::Display};

/// The error type of the crate.
#[derive(Debug)]
pub enum Error {
    /// Invalid 7z signature found in file header.
    BadSignature([u8; 6]),
    /// Unsupported 7z format version.
    UnsupportedVersion {
        /// Major version number.
        major: u8,
        /// Minor version number.
        minor: u8,
    },
    /// Checksum verification failed during decompression.
    ChecksumVerificationFailed,
    /// Next header CRC mismatch.
    NextHeaderCrcMismatch,
    /// IO error with optional context message.
    Io(std::io::Error, Cow<'static, str>),
    /// Error opening file.
    FileOpen(std::io::Error, String),
    /// Other error with description.
    Other(Cow<'static, str>),
    /// Bad terminated streams info.
    BadTerminatedStreamsInfo(u8),
    /// Bad terminated unpack info.
    BadTerminatedUnpackInfo,
    /// Bad terminated pack info.
    BadTerminatedPackInfo(u8),
    /// Bad terminated sub streams info.
    BadTerminatedSubStreamsInfo,
    /// Bad terminated header.
    BadTerminatedHeader(u8),
    /// External compression method not supported.
    ExternalUnsupported,
    /// Unsupported compression method.
    UnsupportedCompressionMethod(String),
    /// Memory limit exceeded.
    MaxMemLimited {
        /// Maximum allowed memory in KB.
        max_kb: usize,
        /// Actual required memory in KB.
        actaul_kb: usize,
    },
    /// Password required for encrypted archive.
    PasswordRequired,
    /// Feature or operation not supported.
    Unsupported(Cow<'static, str>),
    /// Possibly bad password for encrypted content.
    MaybeBadPassword(std::io::Error),
    /// File not found.
    FileNotFound,
    /// The archive's declared end header is larger than the caller's limit.
    ///
    /// The size comes out of the first 32 bytes of the file and the reader has
    /// to buffer that many bytes to parse the header, so it is checked before
    /// the allocation rather than after it. See [`ArchiveLimits`].
    ///
    /// [`ArchiveLimits`]: crate::ArchiveLimits
    EndHeaderTooLarge {
        /// The caller's limit, in bytes.
        limit_bytes: u64,
        /// What the archive declared, in bytes.
        declared_bytes: u64,
    },
    /// The archive's decoders need more memory than the caller's limit.
    ///
    /// Raised by [`ArchiveReader::with_limits`] from the estimate in
    /// [`Archive::decoder_memory_estimate`], before a dictionary is allocated.
    ///
    /// [`ArchiveReader::with_limits`]: crate::ArchiveReader::with_limits
    /// [`Archive::decoder_memory_estimate`]: crate::Archive::decoder_memory_estimate
    MemoryLimited {
        /// The caller's limit, in bytes.
        limit_bytes: u64,
        /// What the archive's coders need, in bytes.
        required_bytes: u64,
    },
    /// A block failed to decode, with enough context to say which bytes.
    ///
    /// This is what separates "this archive is damaged, and here is where" from
    /// an I/O failure on the source or a method this build cannot decode; the
    /// [`kind`](BlockErrorKind) says which, and `packed_offset` is the absolute
    /// file offset of the block's first packed stream, so a caller can name the
    /// damaged region without re-parsing the header.
    BlockDecode {
        /// Index of the block (7-Zip calls it a folder) in [`Archive::blocks`].
        ///
        /// [`Archive::blocks`]: crate::Archive::blocks
        block_index: usize,
        /// Absolute offset of the block's first packed stream in the archive.
        packed_offset: u64,
        /// What went wrong.
        kind: BlockErrorKind,
        /// The underlying error, already rendered.
        message: String,
    },
}

/// What went wrong in an [`Error::BlockDecode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockErrorKind {
    /// The packed bytes are not what the coder expected: the archive is
    /// damaged or truncated, or it is not the archive the caller thinks.
    Corrupted,
    /// The block decoded, but its CRC-32 does not match what the header says.
    ChecksumMismatch,
    /// The block uses a coder this build cannot decode. Not a defect in the
    /// archive: enable the feature, or use a different reader.
    UnsupportedMethod,
    /// The source reader failed. Nothing is known about the archive from this.
    Io,
    /// The password is missing or wrong.
    Password,
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::io_msg(value, "")
    }
}

impl Error {
    #[inline]
    pub(crate) fn other<S: Into<Cow<'static, str>>>(s: S) -> Self {
        Self::Other(s.into())
    }

    #[inline]
    pub(crate) fn unsupported<S: Into<Cow<'static, str>>>(s: S) -> Self {
        Self::Unsupported(s.into())
    }

    #[inline]
    pub(crate) fn io_msg(e: std::io::Error, msg: impl Into<Cow<'static, str>>) -> Self {
        Self::Io(e, msg.into())
    }

    pub(crate) fn bad_password(e: std::io::Error, encryped: bool) -> Self {
        if encryped {
            Self::MaybeBadPassword(e)
        } else {
            Self::io_msg(e, "")
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[inline]
    pub(crate) fn file_open(e: std::io::Error, filename: impl Into<Cow<'static, str>>) -> Self {
        Self::Io(e, filename.into())
    }

    pub(crate) fn maybe_bad_password(self, encryped: bool) -> Self {
        if !encryped {
            return self;
        }
        match self {
            Self::Io(e, s) if s.is_empty() => Self::MaybeBadPassword(e),
            _ => self,
        }
    }
}

impl Error {
    /// Adds block context to an error raised while decoding that block.
    pub(crate) fn in_block(self, block_index: usize, packed_offset: u64) -> Self {
        // Already located; do not re-wrap an inner block's context away.
        if matches!(self, Self::BlockDecode { .. }) {
            return self;
        }
        let kind = match &self {
            Self::Io(..) | Self::FileOpen(..) => BlockErrorKind::Io,
            Self::UnsupportedCompressionMethod(..)
            | Self::Unsupported(..)
            | Self::ExternalUnsupported => BlockErrorKind::UnsupportedMethod,
            Self::ChecksumVerificationFailed | Self::NextHeaderCrcMismatch => {
                BlockErrorKind::ChecksumMismatch
            }
            Self::PasswordRequired | Self::MaybeBadPassword(..) => BlockErrorKind::Password,
            _ => BlockErrorKind::Corrupted,
        };
        Self::BlockDecode {
            block_index,
            packed_offset,
            kind,
            message: self.to_string(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self, f)
    }
}

impl std::error::Error for Error {}
