// Copyright 2021 The Grin Developers
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::client_utils::{Client, ClientError};
use crate::libwallet::slate_versions::{SlateVersion, VersionedSlate};
use crate::libwallet::{Error, Slate};
use crate::SlateSender;

use grin_p2p::tor::arti::tor_post;
use grin_p2p::tor::config::{TorConfig, REQUEST_TIMEOUT_SECS};
use grin_p2p::tor::process::types::TorProcess;
use grin_p2p::tor::start_tor;
use grin_wallet_config::TorWalletConfig;
use serde::Serialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct TorSlateSender {
	base_url: String,
	tor_process: Arc<Option<TorProcess>>,
}

impl TorSlateSender {
	/// Create, return Err if scheme is not "http"
	pub fn new(base_url: &str, config: &TorWalletConfig) -> Result<TorSlateSender, Error> {
		if !base_url.starts_with("http") && !base_url.starts_with("https") {
			Err(Error::GenericError("Scheme must be http".to_string()))
		} else {
			let tor = start_tor(config)?;
			Ok(TorSlateSender {
				base_url: base_url.to_owned(),
				tor_process: Arc::new(tor),
			})
		}
	}

	/// Check version of the listening wallet
	pub fn check_other_version(
		&mut self,
		url: &str,
		config: &TorWalletConfig,
	) -> Result<SlateVersion, Error> {
		let req = json!({
			"jsonrpc": "2.0",
			"method": "check_version",
			"id": 1,
			"params": []
		});

		let res: String = self.post(url, req, config).map_err(|e| {
			let mut report = format!("Performing version check (is recipient listening?): {}", e);
			let err_string = format!("{}", e);
			if !config.use_integrated() && err_string.contains("404") {
				// Report that the other version of the wallet is out of date
				report = "Other wallet is incompatible and requires an upgrade. \
				          Please urge the other wallet owner to upgrade and try the transaction again."
					.to_string();
				error!("{}", report);
			}
			Error::ClientCallback(report)
		})?;

		let res: Value = serde_json::from_str(&res)
			.map_err(|_| Error::ClientCallback(format!("Can not parse response {:?}", res)))?;
		trace!("Response: {}", res);
		if res["error"] != json!(null) {
			let report = format!(
				"Posting transaction slate: Error: {}, Message: {}",
				res["error"]["code"], res["error"]["message"]
			);
			error!("{}", report);
			return Err(Error::ClientCallback(report));
		}

		let resp_value = res["result"]["Ok"].clone();
		trace!("resp_value: {}", resp_value.clone());
		let foreign_api_version: u16 =
			serde_json::from_value(resp_value["foreign_api_version"].clone())
				.map_err(|_| Error::ClientCallback(format!("Can not parse response {:?}", res)))?;
		let supported_slate_versions: Vec<String> =
			serde_json::from_value(resp_value["supported_slate_versions"].clone())
				.map_err(|_| Error::ClientCallback(format!("Can not parse response {:?}", res)))?;

		// trivial tests for now, but will be expanded later
		if foreign_api_version < 2 {
			let report = "Other wallet reports unrecognized API format.".to_string();
			error!("{}", report);
			return Err(Error::ClientCallback(report));
		}

		if supported_slate_versions.contains(&"V4".to_owned()) {
			return Ok(SlateVersion::V4);
		}

		let report = "Unable to negotiate slate format with other wallet.".to_string();
		error!("{}", report);
		Err(Error::ClientCallback(report))
	}

	fn post<IN>(
		&self,
		url: &str,
		input: IN,
		config: &TorWalletConfig,
	) -> Result<String, ClientError>
	where
		IN: Serialize,
	{
		let res = if self.tor_process.is_some() {
			let socks_proxy_addr =
				SocketAddr::V4(config.socks_proxy_addr.parse().map_err(|_| {
					ClientError::Internal("Socks proxy address is not set".to_string())
				})?);
			let timeout = Duration::from_secs(
				config
					.request_timeout_secs()
					.filter(|s| *s > 0)
					.unwrap_or(REQUEST_TIMEOUT_SECS),
			);
			let client = Client::with_proxy(socks_proxy_addr, "socks5h://", timeout)
				.map_err(|_| ClientError::Internal("Unable to create http client".into()))?;
			let req = client.create_post_request(url, None, &input)?;
			client.send_request(req)?
		} else {
			tor_post(config, &input, url).map_err(|e| ClientError::RequestError(e.to_string()))?
		};
		Ok(res)
	}
}

impl SlateSender for TorSlateSender {
	fn send_tx(
		&mut self,
		slate: &Slate,
		config: &TorWalletConfig,
		finalize: bool,
	) -> Result<Slate, Error> {
		let trailing = match self.base_url.ends_with('/') {
			true => "",
			false => "/",
		};
		let url_str = format!("{}{}v2/foreign", self.base_url, trailing);

		let slate_send = match self.check_other_version(&url_str, config)? {
			SlateVersion::V4 => VersionedSlate::into_version(slate.clone(), SlateVersion::V4)?,
		};
		// Note: not using easy-jsonrpc as don't want the dependencies in this crate
		let req = match finalize {
			false => json!({
				"jsonrpc": "2.0",
				"method": "receive_tx",
				"id": 1,
				"params": [
							slate_send,
							null,
							null
						]
			}),
			true => json!({
				"jsonrpc": "2.0",
				"method": "finalize_tx",
				"id": 1,
				"params": [
							slate_send
						]
			}),
		};

		trace!("Sending receive_tx request: {}", req);

		let res: String = self.post(&url_str, req, config).map_err(|e| {
			let report = format!(
				"Sending transaction slate to other wallet (is recipient listening?): {}",
				e
			);
			Error::ClientCallback(report)
		})?;

		let res: Value = serde_json::from_str(&res)
			.map_err(|_| Error::ClientCallback(format!("Can not parse response {:?}", res)))?;
		trace!("Response: {}", res);
		if res["error"] != json!(null) {
			let report = format!(
				"Posting transaction slate: Error: {}, Message: {}",
				res["error"]["code"], res["error"]["message"]
			);
			error!("{}", report);
			return Err(Error::ClientCallback(report));
		}

		let slate_value = res["result"]["Ok"].clone();

		trace!("slate_value: {}", slate_value);
		if let Ok(slate_value) = serde_json::to_string(&slate_value) {
			let slate = Slate::deserialize_upgrade(&slate_value).map_err(|e| {
				error!("Error deserializing response slate: {}", e);
				Error::SlateDeser
			})?;
			Ok(slate)
		} else {
			error!("Error deserializing response slate: {}", slate_value);
			Err(Error::SlateDeser)
		}
	}
}
