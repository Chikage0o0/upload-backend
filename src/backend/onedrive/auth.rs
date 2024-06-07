use std::{
    io::{BufRead as _, BufReader, Write as _},
    net::TcpListener,
    path,
    sync::{atomic::AtomicU64, Arc},
};

use super::{ApiType, Error, OnedriveInner};
use arc_swap::ArcSwap;
use oauth2::{
    basic::{BasicClient, BasicErrorResponseType, BasicTokenType},
    reqwest::async_http_client,
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EmptyExtraTokenFields,
    PkceCodeChallenge, RedirectUrl, RefreshToken, RevocationErrorResponseType, Scope,
    StandardErrorResponse, StandardRevocableToken, StandardTokenIntrospectionResponse,
    StandardTokenResponse, TokenResponse as _, TokenUrl,
};
use reqwest::Url;

pub type Client = oauth2::Client<
    StandardErrorResponse<BasicErrorResponseType>,
    StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>,
    BasicTokenType,
    StandardTokenIntrospectionResponse<EmptyExtraTokenFields, BasicTokenType>,
    StandardRevocableToken,
    StandardErrorResponse<RevocationErrorResponseType>,
>;

impl OnedriveInner {
    /// Create a new OneDrive client using a refresh token.
    /// This is useful for long-running applications that need to refresh the token.
    pub async fn new_with_refresh_token(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        refresh_token: impl Into<String>,
        api_type: ApiType,
        path: impl AsRef<path::Path>,
    ) -> Result<Self, Error> {
        if !path.as_ref().has_root() {
            return Err(Error::InvalidPath {
                path: path.as_ref().to_string_lossy().to_string(),
            });
        }

        let graph_client_id = ClientId::new(client_id.into());
        let graph_client_secret = ClientSecret::new(client_secret.into());
        let auth_url = AuthUrl::new(api_type.get_auth_url().to_string())
            .expect("Invalid authorization endpoint URL");
        let token_url = TokenUrl::new(api_type.get_token_url().to_string())
            .expect("Invalid token endpoint URL");

        // Set up the config for the Microsoft Graph OAuth2 process.
        let client: Client = BasicClient::new(
            graph_client_id,
            Some(graph_client_secret),
            auth_url,
            Some(token_url),
        )
        .set_auth_type(AuthType::RequestBody);

        let token_result = client
            .exchange_refresh_token(&RefreshToken::new(refresh_token.into()))
            .request_async(async_http_client)
            .await
            .map_err(|e| Error::RefreshToken {
                message: e.to_string(),
            })?;

        Ok(Self::new(client, api_type, token_result, path))
    }

    pub async fn refresh(&self) -> Result<(), Error> {
        let token_result = self
            .client
            .exchange_refresh_token(&RefreshToken::new(self.refresh_token.load().to_string()))
            .request_async(async_http_client)
            .await
            .map_err(|e| Error::RefreshToken {
                message: e.to_string(),
            })?;

        self.access_token
            .store(Arc::new(token_result.access_token().secret().to_string()));
        self.refresh_token.store(Arc::new(
            token_result.refresh_token().unwrap().secret().to_string(),
        ));
        self.expires_at.store(
            calu_expires_at(token_result.expires_in().unwrap().as_secs()),
            std::sync::atomic::Ordering::Release,
        );

        Ok(())
    }

    pub async fn new_with_code(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect_url: impl Into<String>,
        api_type: ApiType,
        path: impl AsRef<path::Path>,
    ) -> Result<Self, Error> {
        if !path.as_ref().has_root() {
            return Err(Error::InvalidPath {
                path: path.as_ref().to_string_lossy().to_string(),
            });
        }

        let graph_client_id = ClientId::new(client_id.into());
        let graph_client_secret = ClientSecret::new(client_secret.into());
        let auth_url = AuthUrl::new(api_type.get_auth_url().to_string())
            .expect("Invalid authorization endpoint URL");
        let token_url = TokenUrl::new(api_type.get_token_url().to_string())
            .expect("Invalid token endpoint URL");

        // Set up the config for the Microsoft Graph OAuth2 process.
        let client: Client = BasicClient::new(
            graph_client_id,
            Some(graph_client_secret),
            auth_url,
            Some(token_url),
        )
        .set_auth_type(AuthType::RequestBody)
        // This example will be running its own server at localhost:3003.
        // See below for the server implementation.
        .set_redirect_uri(RedirectUrl::new(redirect_url.into()).expect("Invalid redirect URL"));

        let (pkce_code_challenge, pkce_code_verifier) = PkceCodeChallenge::new_random_sha256();

        // Generate the authorization URL to which we'll redirect the user.
        let (authorize_url, csrf_state) = client
            .authorize_url(CsrfToken::new_random)
            // This example requests read access to OneDrive.
            .add_scope(Scope::new("files.readwrite".to_string()))
            .add_scope(Scope::new("offline_access".to_string()))
            .set_pkce_challenge(pkce_code_challenge)
            .url();

        println!("Open this URL in your browser:\n{}\n", authorize_url);

        let (code, state) = {
            // A very naive implementation of the redirect server.
            let listener = TcpListener::bind("0.0.0.0:20080").unwrap();

            // The server will terminate itself after collecting the first code.
            let Some(mut stream) = listener.incoming().flatten().next() else {
                panic!("listener terminated without accepting a connection");
            };

            let mut reader = BufReader::new(&stream);

            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();

            let redirect_url = request_line.split_whitespace().nth(1).unwrap();
            let url = Url::parse(&("http://localhost".to_string() + redirect_url)).unwrap();

            let code = url
                .query_pairs()
                .find(|(key, _)| key == "code")
                .map(|(_, code)| AuthorizationCode::new(code.into_owned()))
                .unwrap();

            let state = url
                .query_pairs()
                .find(|(key, _)| key == "state")
                .map(|(_, state)| CsrfToken::new(state.into_owned()))
                .unwrap();

            let message = "Go back to your terminal :)";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
                message.len(),
                message
            );
            stream.write_all(response.as_bytes()).unwrap();

            (code, state)
        };

        if state.secret() != csrf_state.secret() {
            return Err(Error::CsrfToken);
        }

        let token_result = client
            .exchange_code(code)
            // Set the PKCE code verifier.
            .set_pkce_verifier(pkce_code_verifier)
            .request_async(async_http_client)
            .await
            .map_err(|e| Error::RefreshToken {
                message: e.to_string(),
            })?;

        Ok(Self::new(client, api_type, token_result, path))
    }

    fn new(
        client: Client,
        api_type: ApiType,
        response: StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>,
        path: impl AsRef<path::Path>,
    ) -> Self {
        let access_token = response.access_token().secret().to_string();
        let refresh_token = response.refresh_token().unwrap().secret().to_string();
        let expires_at = calu_expires_at(response.expires_in().unwrap().as_secs());
        OnedriveInner {
            client,
            access_token: ArcSwap::from_pointee(access_token),
            refresh_token: ArcSwap::from_pointee(refresh_token),
            expires_at: AtomicU64::new(expires_at),
            api_type,
            folder: path.as_ref().to_path_buf(),
        }
    }
}

fn calu_expires_at(expires_in: u64) -> u64 {
    let now = chrono::Utc::now().timestamp() as u64;
    now + expires_in
}
