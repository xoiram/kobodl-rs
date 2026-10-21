//! Reads the login from the settings files of the Python tools, so it can be imported instead of logging in again.
//!
//! Supported formats:
//!
//! - TnS-hun's kobo-book-downloader (`~/.config/kobo-book-downloader.json`): one object with the credential fields.
//! - kobodl (`~/.config/kobodl.json`): `{"users": [...]}` with one object per account, or one of those objects.

use serde_json::Value;

use crate::client::Credentials;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImportError {
    #[error("this isn't valid JSON")]
    InvalidJson,

    #[error("this isn't a kobo-book-downloader or kobodl settings file")]
    UnknownFormat,

    #[error("the file has no accounts")]
    NoAccounts,

    #[error("the file has {0} accounts; paste only the one you want from the \"users\" list")]
    MultipleAccounts(usize),

    #[error(
        "the login in the file is incomplete, {0} is missing; log in with the tool first, or log in with a code here"
    )]
    MissingField(&'static str),
}

/// Parses the contents of a kobo-book-downloader or kobodl settings file.
pub fn credentials_from_settings(json: &str) -> Result<Credentials, ImportError> {
    let value: Value = serde_json::from_str(json).map_err(|_| ImportError::InvalidJson)?;
    let Value::Object(object) = value else {
        return Err(ImportError::UnknownFormat);
    };

    let account = match object.get("users") {
        Some(Value::Array(users)) => match users.as_slice() {
            [] => return Err(ImportError::NoAccounts),
            [Value::Object(user)] => user,
            [_] => return Err(ImportError::UnknownFormat),
            users => return Err(ImportError::MultipleAccounts(users.len())),
        },
        Some(_) => return Err(ImportError::UnknownFormat),
        None => &object,
    };
    if !account.contains_key("DeviceId") && !account.contains_key("RefreshToken") {
        return Err(ImportError::UnknownFormat);
    }

    let field = |name: &str| account.get(name).and_then(Value::as_str).unwrap_or_default().to_owned();
    let credentials = Credentials {
        device_id: field("DeviceId"),
        serial_number: field("SerialNumber"),
        access_token: field("AccessToken"),
        refresh_token: field("RefreshToken"),
        user_id: field("UserId"),
        user_key: field("UserKey"),
        email: field("Email"),
    };

    let required: [(&'static str, &str); 5] = [
        ("DeviceId", &credentials.device_id),
        ("AccessToken", &credentials.access_token),
        ("RefreshToken", &credentials.refresh_token),
        ("UserId", &credentials.user_id),
        ("UserKey", &credentials.user_key),
    ];
    if let Some((name, _)) = required.iter().find(|(_, value)| value.is_empty()) {
        return Err(ImportError::MissingField(name));
    }
    Ok(credentials)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn account() -> Value {
        json!({
            "AccessToken": "access",
            "DeviceId": "device",
            "RefreshToken": "refresh",
            "SerialNumber": "serial",
            "UserId": "user",
            "UserKey": "key",
        })
    }

    #[test]
    fn reads_kobo_book_downloader_settings() {
        let credentials = credentials_from_settings(&account().to_string()).unwrap();
        assert_eq!(credentials.device_id, "device");
        assert_eq!(credentials.serial_number, "serial");
        assert_eq!(credentials.access_token, "access");
        assert_eq!(credentials.refresh_token, "refresh");
        assert_eq!(credentials.user_id, "user");
        assert_eq!(credentials.user_key, "key");
        assert_eq!(credentials.email, "");
    }

    #[test]
    fn reads_kobodl_settings() {
        let mut user = account();
        user["Email"] = "reader@example.com".into();

        let credentials = credentials_from_settings(&json!({ "users": [user.clone()] }).to_string()).unwrap();
        assert_eq!(credentials.email, "reader@example.com");
        assert_eq!(credentials.user_key, "key");

        // A single entry copied out of the users list.
        assert_eq!(credentials_from_settings(&user.to_string()).unwrap(), credentials);
    }

    #[test]
    fn explains_what_is_wrong() {
        let parse = |value: Value| credentials_from_settings(&value.to_string()).unwrap_err();

        assert_eq!(
            credentials_from_settings("{not json").unwrap_err(),
            ImportError::InvalidJson
        );
        assert_eq!(parse(json!([1, 2])), ImportError::UnknownFormat);
        assert_eq!(parse(json!({ "theme": "dark" })), ImportError::UnknownFormat);
        assert_eq!(parse(json!({ "users": [] })), ImportError::NoAccounts);
        assert_eq!(
            parse(json!({ "users": [account(), account()] })),
            ImportError::MultipleAccounts(2)
        );

        let mut logged_out = account();
        logged_out["UserKey"] = "".into();
        assert_eq!(parse(logged_out), ImportError::MissingField("UserKey"));
    }
}
