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
