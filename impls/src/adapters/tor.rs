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

use crate::adapters::types::HttpSlateSender;
use crate::client_utils::Client;
use crate::libwallet::Error;

use grin_p2p::tor::arti::tor_post;
use grin_p2p::tor::config::{TorConfig, REQUEST_TIMEOUT_SECS};
use grin_p2p::tor::process::types::TorProcess;
use grin_p2p::tor::start_tor;

use grin_p2p::tor::ov3::OnionV3Address;
use grin_wallet_config::TorWalletConfig;
use grin_wallet_libwallet::SlatepackAddress;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct TorSlateSender {
	host_url: String,
	tor_process: Arc<Option<TorProcess>>,
	tor_config: TorWalletConfig,
}

impl TorSlateSender {
	/// Create instance from receiver address and Tor config, return Error if Tor can not be started.
	pub fn new(
		dest: &SlatepackAddress,
		tor_config: TorWalletConfig,
	) -> Result<TorSlateSender, Error> {
		let tor_addr = OnionV3Address::from(dest);
		let host_url = tor_addr.to_http_str();
		let tor = start_tor(&tor_config)?;
		Ok(TorSlateSender {
			host_url,
			tor_process: Arc::new(tor),
			tor_config,
		})
	}
}

impl HttpSlateSender for TorSlateSender {
	fn host_url(&self) -> String {
		self.host_url.clone()
	}

	fn post(&self, url: &str, body: String) -> Result<String, Error> {
		let res = if self.tor_process.is_some() {
			let timeout = Duration::from_secs(
				self.tor_config
					.request_timeout_secs()
					.filter(|s| *s > 0)
					.unwrap_or(REQUEST_TIMEOUT_SECS),
			);
			let socks_proxy_addr = SocketAddr::V4(
				self.tor_config
					.socks_proxy_addr()
					.parse()
					.map_err(|e| Error::TorConfig(format!("{:?}", e)))?,
			);
			let client = Client::with_proxy(socks_proxy_addr, "socks5h://", timeout)
				.map_err(|e| Error::Tor(format!("Unable to create proxy client {}", e)))?;
			let req = client
				.create_post_request(url, None, &body)
				.map_err(|e| Error::Tor(format!("Unable to create request {}", e)))?;
			client
				.send_request(req)
				.map_err(|e| Error::Tor(format!("Unable to send request {}", e)))?
		} else {
			let mut headers = HashMap::new();
			headers.insert("accept", "application/json");
			headers.insert("content-type", "application/json");
			tor_post(&self.tor_config, body, Some(headers), url)
				.map_err(|e| Error::Tor(e.to_string()))?
		};
		Ok(res)
	}
}
