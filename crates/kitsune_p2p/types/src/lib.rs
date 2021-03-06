#![deny(missing_docs)]
//! Types subcrate for kitsune-p2p.

/// Re-exported dependencies.
pub mod dependencies {
    pub use ::futures;
    pub use ::ghost_actor;
    pub use ::thiserror;
    pub use ::tokio;
    pub use ::url2;
}

pub mod async_lazy;
pub mod dht_arc;

/// A collection of definitions related to remote communication.
pub mod transport {
    /// Error related to remote communication.
    #[derive(Debug, thiserror::Error)]
    #[non_exhaustive]
    pub enum TransportError {
        /// GhostError.
        #[error(transparent)]
        GhostError(#[from] ghost_actor::GhostError),

        /// Unspecified error.
        #[error(transparent)]
        Other(Box<dyn std::error::Error + Send + Sync>),
    }

    impl TransportError {
        /// promote a custom error type to a TransportError
        pub fn other(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
            Self::Other(e.into())
        }
    }

    impl From<String> for TransportError {
        fn from(s: String) -> Self {
            #[derive(Debug, thiserror::Error)]
            struct OtherError(String);
            impl std::fmt::Display for OtherError {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(f, "{}", self.0)
                }
            }

            TransportError::other(OtherError(s))
        }
    }

    impl From<&str> for TransportError {
        fn from(s: &str) -> Self {
            s.to_string().into()
        }
    }

    impl From<TransportError> for () {
        fn from(_: TransportError) {}
    }

    /// Result type for remote communication.
    pub type TransportResult<T> = Result<T, TransportError>;

    /// Defines an established connection to a remote peer.
    pub mod transport_connection {
        ghost_actor::ghost_chan! {
            /// Event stream for handling incoming requests from a remote.
            pub chan TransportConnectionEvent<super::TransportError> {
                /// Event for handling incoming requests from a remote.
                fn incoming_request(url: url2::Url2, data: Vec<u8>) -> Vec<u8>;
            }
        }

        /// Receiver type for incoming connection events.
        pub type TransportConnectionEventReceiver =
            futures::channel::mpsc::Receiver<TransportConnectionEvent>;

        ghost_actor::ghost_chan! {
            /// Represents a connection to a remote node.
            pub chan TransportConnection<super::TransportError> {
                /// Retrieve the current url (address) of the remote end of this connection.
                fn remote_url() -> url2::Url2;

                /// Make a request of the remote end of this connection.
                fn request(data: Vec<u8>) -> Vec<u8>;
            }
        }
    }

    /// Defines a local binding
    /// (1) for accepting incoming connections and
    /// (2) for making outgoing connections.
    pub mod transport_listener {
        ghost_actor::ghost_chan! {
            /// Event stream for handling incoming connections.
            pub chan TransportListenerEvent<super::TransportError> {
                /// Event for handling incoming connections from a remote.
                fn incoming_connection(
                    sender: ghost_actor::GhostSender<super::transport_connection::TransportConnection>,
                    receiver: super::transport_connection::TransportConnectionEventReceiver,
                ) -> ();
            }
        }

        /// Receiver type for incoming listener events.
        pub type TransportListenerEventReceiver =
            futures::channel::mpsc::Receiver<TransportListenerEvent>;

        ghost_actor::ghost_chan! {
            /// Represents a socket binding for establishing connections.
            pub chan TransportListener<super::TransportError> {
                /// Retrieve the current url (address) this listener is bound to.
                fn bound_url() -> url2::Url2;

                /// Attempt to establish an outgoing connection to a remote.
                fn connect(url: url2::Url2) -> (
                    ghost_actor::GhostSender<super::transport_connection::TransportConnection>,
                    super::transport_connection::TransportConnectionEventReceiver,
                );
            }
        }
    }
}
