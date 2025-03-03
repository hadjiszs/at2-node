//! Client for connecting to an AT2 node

use drop::crypto::sign;
use http::Uri;
use snafu::{ResultExt, Snafu};

use crate::{
    proto::{at2_client::At2Client, *},
    FullTransaction, ThinTransaction,
};

/// Error generated by this client
#[derive(Debug, Snafu)]
pub enum Error {
    /// Connecting to the server
    #[cfg(not(target_family = "wasm"))]
    Transport {
        /// Source of the error
        source: tonic::transport::Error,
    },
    /// Deserialize the server's reply
    Deserialize {
        /// Source of the error
        source: bincode::Error,
    },
    /// Deserializing the timestamp
    DeserializeTimestamp {
        /// Source of the error
        source: chrono::ParseError,
    },
    /// Serializing the server's query
    Serialize {
        /// Source of the error
        source: bincode::Error,
    },
    /// Communicating with the server
    Rpc {
        /// Source of the error
        source: tonic::Status,
    },
}

type Result<T> = std::result::Result<T, Error>;

/// gRPC web client for the node
#[derive(Clone)]
pub struct Client(
    #[cfg(target_family = "wasm")] At2Client<grpc_web_client::Client>,
    #[cfg(not(target_family = "wasm"))] At2Client<tonic::transport::Channel>,
);

impl Client {
    /// Create a new client connecting to the given [`Uri`]
    pub fn new(uri: Uri) -> Result<Self> {
        let mut url_string = uri.to_string();
        if uri.path() == "/" {
            // TODO fix upstream handling
            url_string.pop();
        }

        #[cfg(target_family = "wasm")]
        let connection = grpc_web_client::Client::new(url_string);
        #[cfg(not(target_family = "wasm"))]
        let connection = tonic::transport::Channel::builder(uri)
            .connect_lazy()
            .context(Transport)?;

        Ok(Self(At2Client::new(connection)))
    }

    /// Send a given number of asset to the given user.
    ///
    /// `sequence` is counter used by the sender.
    /// You should increase it by one for each new transaction you want to send.
    pub async fn send_asset(
        &mut self,
        user: &sign::KeyPair,
        sequence: sieve::Sequence,
        recipient: sign::PublicKey,
        amount: u64,
    ) -> Result<()> {
        let message = ThinTransaction { recipient, amount };
        let signature = user.sign(&message).expect("sign failed");

        self.0
            .send_asset(tonic::Request::new(SendAssetRequest {
                sender: bincode::serialize(&user.public()).context(Serialize)?,
                sequence,
                recipient: bincode::serialize(&recipient).context(Serialize)?,
                amount,
                signature: bincode::serialize(&signature).context(Serialize)?,
            }))
            .await
            .context(Rpc)
            .map(|_| ())
    }

    /// Return the balance of the user
    pub async fn get_balance(&mut self, user: &sign::PublicKey) -> Result<u64> {
        self.0
            .get_balance(tonic::Request::new(GetBalanceRequest {
                sender: bincode::serialize(user).context(Serialize)?,
            }))
            .await
            .context(Rpc)
            .map(|reply| reply.get_ref().amount)
    }

    /// Get the latest used sequence
    pub async fn get_last_sequence(&mut self, user: &sign::PublicKey) -> Result<sieve::Sequence> {
        self.0
            .get_last_sequence(tonic::Request::new(GetLastSequenceRequest {
                sender: bincode::serialize(user).context(Serialize)?,
            }))
            .await
            .context(Rpc)
            .map(|reply| reply.get_ref().sequence)
    }

    /// Get the number of recently processed transactions
    pub async fn get_latest_transactions(&mut self) -> Result<Vec<FullTransaction>> {
        self.0
            .get_latest_transactions(tonic::Request::new(GetLatestTransactionsRequest {}))
            .await
            .context(Rpc)?
            .into_inner()
            .transactions
            .iter()
            .map(|tx| {
                Ok(FullTransaction {
                    timestamp: chrono::DateTime::parse_from_rfc3339(&tx.timestamp)
                        .context(DeserializeTimestamp)?
                        .into(),
                    sender: bincode::deserialize(&tx.sender).context(Deserialize)?,
                    recipient: bincode::deserialize(&tx.recipient).context(Deserialize)?,
                    amount: tx.amount,
                })
            })
            .collect()
    }
}
