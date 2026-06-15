#[derive(Debug)]
pub enum Error {
    ReqwestError(reqwest::Error),
    TelegramError {
        error_code: i32,
        description: String,
    },
    SerdeXMLError(serde_xml_rs::Error),
    InvalidRedditAccessToken,
    Custom(String),
    RhaiEvalAltResult(Box<rhai::EvalAltResult>),
    IOError(std::io::Error),
    SQLiteError(sqlite::Error),
}

impl From<reqwest::Error> for Error {
    fn from(value: reqwest::Error) -> Self {
        Self::ReqwestError(value)
    }
}

impl From<serde_xml_rs::Error> for Error {
    fn from(value: serde_xml_rs::Error) -> Self {
        Self::SerdeXMLError(value)
    }
}

impl From<String> for Error {
    fn from(value: String) -> Self {
        Self::Custom(value)
    }
}

impl From<Box<rhai::EvalAltResult>> for Error {
    fn from(value: Box<rhai::EvalAltResult>) -> Self {
        Self::RhaiEvalAltResult(value)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::IOError(value)
    }
}

impl From<sqlite::Error> for Error {
    fn from(value: sqlite::Error) -> Self {
        Self::SQLiteError(value)
    }
}
