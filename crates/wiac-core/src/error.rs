use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("dxf: {0}")]
    Dxf(#[from] dxf::DxfError),

    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("malformed input: {0}")]
    Malformed(String),

    #[error("parse: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, Error>;
