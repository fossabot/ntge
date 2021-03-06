pub mod decryptor;
pub mod encryptor;
pub mod recipient;

use bs58;
use bson;
use serde::{Deserialize, Serialize};
use serde_bytes;

#[cfg(target_os = "ios")]
use crate::strings;

#[cfg(target_os = "ios")]
use std::os::raw::c_char;

use crate::{error::CoreError, message::recipient::MessageRecipientHeader};

pub(crate) const MAC_KEY_LABEL: &[u8] = b"ntge-message-mac-key";
pub(crate) const PAYLOAD_KEY_LABEL: &[u8] = b"ntge-message-payload";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageMeta {
    pub timestamp: Option<String>,
    pub signature: Option<encryptor::Signature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageMac {
    #[serde(with = "serde_bytes")]
    pub mac: Vec<u8>, // 32 bytes
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePayload {
    #[serde(with = "serde_bytes")]
    pub nonce: Vec<u8>, // 16 bytes
    #[serde(with = "serde_bytes")]
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub recipient_headers: Vec<MessageRecipientHeader>,
    pub meta: MessageMeta,
    pub mac: MessageMac,
    pub payload: MessagePayload,
}

#[allow(dead_code)]
impl Message {
    pub fn serialize_to_armor(&self) -> Result<String, CoreError> {
        match self.serialize_to_base58() {
            Ok(base58) => Ok(format!("MsgBegin_{}_EndMsg", base58)),
            Err(e) => Err(e),
        }
    }

    pub fn deserialize_from_armor(text: &str) -> Result<Message, CoreError> {
        let text = text.trim();
        let has_prefix = text.starts_with("MsgBegin_");
        let has_suffix = text.ends_with("_EndMsg");

        if has_prefix && has_suffix {
            let text = text.trim_start_matches("MsgBegin_");
            let text = text.trim_end_matches("_EndMsg");
            Message::deserialize_from_base58(text)
        } else {
            // no armor
            let e = CoreError::MessageSerializationError {
                name: "Message",
                reason: "cannot deserialize message armor text",
            };
            Err(e)
        }
    }
}

impl Message {
    pub fn serialize_to_base58(&self) -> Result<String, CoreError> {
        self.serialize_to_bson_bytes()
            .map(|bytes| bs58::encode(bytes).into_string())
            .map_err(|_| CoreError::MessageSerializationError {
                name: "Message",
                reason: "cannot encode message bytes to base58",
            })
    }

    pub fn deserialize_from_base58(text: &str) -> Result<Message, CoreError> {
        let bytes = match bs58::decode(text).into_vec() {
            Ok(bytes) => bytes,
            Err(_) => {
                let e = CoreError::MessageSerializationError {
                    name: "Message",
                    reason: "cannot decode message base58 text",
                };
                return Err(e);
            }
        };

        Message::deserialize_from_bson_bytes(&bytes)
    }
}

#[allow(dead_code)]
impl Message {
    fn serialize_to_bson_bytes(&self) -> Result<Vec<u8>, CoreError> {
        let document = match bson::to_bson(&self) {
            Ok(encoded) => {
                if let bson::Bson::Document(document) = encoded {
                    document
                } else {
                    let e = CoreError::MessageSerializationError {
                        name: "Message",
                        reason: "cannot encode message to bson",
                    };
                    return Err(e);
                }
            }
            Err(err) => {
                print!("{:?}", err);
                let e = CoreError::MessageSerializationError {
                    name: "Message",
                    reason: "cannot encode message to bson",
                };
                return Err(e);
            }
        };

        let mut bytes = Vec::new();
        match bson::encode_document(&mut bytes, &document) {
            Ok(_) => Ok(bytes),
            Err(_) => {
                let e = CoreError::KeyDeserializeError {
                    name: "Message",
                    reason: "cannot encode message bson document to bytes",
                };
                Err(e)
            }
        }
    }

    fn deserialize_from_bson_bytes(bytes: &Vec<u8>) -> Result<Message, CoreError> {
        let document = match bson::decode_document(&mut std::io::Cursor::new(&bytes[..])) {
            Ok(document) => document,
            Err(_) => {
                let e = CoreError::KeyDeserializeError {
                    name: "Message",
                    reason: "cannot decode bson bytes to message document",
                };
                return Err(e);
            }
        };

        let result: Result<Message, _> = bson::from_bson(bson::Bson::Document(document));
        result.map_err(|_| CoreError::KeyDeserializeError {
            name: "Message",
            reason: "cannot decode message document to message",
        })
    }
}

impl Drop for Message {
    fn drop(&mut self) {
        if cfg!(feature = "drop-log-enable") {
            println!("{:?} is being deallocated", self);
        }
    }
}

#[no_mangle]
#[cfg(target_os = "ios")]
pub unsafe extern "C" fn c_message_destory(message: *mut Message) {
    let _ = Box::from_raw(message);
}

#[no_mangle]
#[cfg(target_os = "ios")]
pub unsafe extern "C" fn c_message_serialize_to_armor(
    message: *mut Message,
    armor: *mut *mut c_char,
) -> i32 {
    let message = &mut *message;
    match message.serialize_to_armor() {
        Ok(text) => {
            let result = strings::string_to_c_char(text);
            *armor = result;
            return 0;
        }
        Err(_) => {
            // TODO:
            return 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        aead,
        ed25519::keypair::Ed25519Keypair,
        message::{self, decryptor, encryptor, Message},
        x25519::{filekey::FileKey, private::X25519PrivateKey, public::X25519PublicKey},
    };

    use hkdf::Hkdf;
    use rand::rngs::OsRng;
    use rand::RngCore;
    use secrecy::ExposeSecret;
    use sha2::Sha256;

    #[allow(dead_code)]
    fn encrypt_plaintext(file_key: &FileKey, plaintext: &[u8]) -> Vec<u8> {
        // prepare nonce
        let mut nonce = [0; 16];
        OsRng.fill_bytes(&mut nonce);
        // create payload key
        let mut payload_key = [0; 32];
        Hkdf::<Sha256>::new(Some(&nonce), file_key.0.expose_secret())
            .expand(message::PAYLOAD_KEY_LABEL, &mut payload_key)
            .expect("payload_key is the correct length");
        aead::aead_encrypt(&payload_key, &plaintext)
    }

    #[test]
    fn it_encrypts_a_message_to_alice() {
        // plaintext
        let plaintext = b"Hello, World!";
        // create new file key
        let file_key = FileKey::new();
        // pass file key to encrypt plaintext
        let ciphertext = encrypt_plaintext(&file_key, plaintext);
        print!("{:?}", ciphertext);
    }

    #[test]
    fn it_encrypts_and_decrypts_a_message_to_alice() {
        let plaintext = b"Hello, World!";
        // alice
        let alice_keypair = Ed25519Keypair::new();
        let alice_secret_key: X25519PrivateKey = (&alice_keypair.get_private_key()).into();
        let alice_public_key: X25519PublicKey = (&alice_keypair.get_public_key()).into();

        let encryptor = encryptor::Encryptor::new(&vec![alice_public_key]);
        let message = encryptor.encrypt(plaintext, None);

        // create decryptor
        let decryptor = decryptor::Decryptor::new(&message);
        // get file key
        let file_key = decryptor
            .decrypt_file_key(&alice_secret_key)
            .expect("could decrypt file key");
        // check mac
        assert_eq!(decryptor.verify_message_mac(&file_key), true);
        // decrypt message payload
        let decrypted_plaintext = decryptor
            .decrypt_payload(&file_key)
            .expect("could decrypt payload");
        assert_eq!(decrypted_plaintext, plaintext);
    }

    #[test]
    fn it_encodes_and_decodes_a_message_to_alice() {
        let plaintext = b"Hello, World!";

        // alice
        let alice_keypair = Ed25519Keypair::new();
        let alice_public_key: X25519PublicKey = (&alice_keypair.get_public_key()).into();

        let encryptor = encryptor::Encryptor::new(&vec![alice_public_key]);
        let message = encryptor.encrypt(plaintext, None);
        // encode
        let encoded_message = message.serialize_to_armor().expect("could serialize");
        print!("{:?}", encoded_message);
        let decoded_message =
            Message::deserialize_from_armor(&encoded_message).expect("could deserialize");
        assert_eq!(decoded_message.mac.mac, message.mac.mac);
        assert_eq!(decoded_message.meta.timestamp, message.meta.timestamp);
    }

    #[test]
    fn it_encrypts_and_decrypts_a_message_to_alice_and_signs() {
        let plaintext = b"Hello, World!";
        // alice
        let alice_keypair = Ed25519Keypair::new();
        let alice_private_key = alice_keypair.get_private_key();
        let alice_public_key = alice_keypair.get_public_key();

        let alice_secret_key_x25519: X25519PrivateKey = (&alice_private_key).into();
        let alice_public_key_x25519: X25519PublicKey = (&alice_public_key).into();

        let encryptor = encryptor::Encryptor::new(&vec![alice_public_key_x25519]);
        let message = encryptor.encrypt(plaintext, Some(&alice_private_key));

        // create decryptor
        let decryptor = decryptor::Decryptor::new(&message);
        // get file key
        let file_key = decryptor
            .decrypt_file_key(&alice_secret_key_x25519)
            .expect("could decrypt file key");
        // check mac
        assert_eq!(decryptor.verify_message_mac(&file_key), true);
        // decrypt message payload
        let decrypted_plaintext = decryptor
            .decrypt_payload(&file_key)
            .expect("could decrypt payload");
        assert_eq!(decrypted_plaintext, plaintext);
    }

    #[test]
    fn it_encrypts_and_decrypts_a_message_to_alice_and_signs_and_verifies() {
        let plaintext = b"Hello, World!";
        // alice
        let alice_keypair = Ed25519Keypair::new();
        let alice_private_key = alice_keypair.get_private_key();
        let alice_public_key = alice_keypair.get_public_key();

        let alice_secret_key_x25519: X25519PrivateKey = (&alice_private_key).into();
        let alice_public_key_x25519: X25519PublicKey = (&alice_public_key).into();

        let encryptor = encryptor::Encryptor::new(&[alice_public_key_x25519]);
        let message = encryptor.encrypt(plaintext, Some(&alice_private_key));

        // create decryptor
        let decryptor = decryptor::Decryptor::new(&message);
        // get file key
        let file_key = decryptor
            .decrypt_file_key(&alice_secret_key_x25519)
            .expect("could decrypt file key");
        // check mac
        assert_eq!(decryptor.verify_message_mac(&file_key), true);
        // decrypt message payload
        let decrypted_plaintext = decryptor
            .decrypt_payload(&file_key)
            .expect("could decrypt payload");
        assert_eq!(decrypted_plaintext, plaintext);
        // verify signature
        assert!(decryptor::Decryptor::verify_signature(
            &message,
            &alice_public_key
        ));
    }

    #[test]
    fn it_encodes_and_decodes_a_signed_message_to_alice() {
        let plaintext = b"Hello, World!";

        // alice
        let alice_keypair = Ed25519Keypair::new();
        let alice_private_key = alice_keypair.get_private_key();
        let alice_public_key = alice_keypair.get_public_key();
        let alice_public_key_x25519: X25519PublicKey = (&alice_public_key).into();

        let encryptor = encryptor::Encryptor::new(&[alice_public_key_x25519]);
        let message = encryptor.encrypt(plaintext, Some(&alice_private_key));
        // encode
        let encoded_message = message.serialize_to_armor().expect("could serialize");
        print!("{:?}", encoded_message);
        let decoded_message =
            Message::deserialize_from_armor(&encoded_message).expect("could deserialize");
        assert_eq!(decoded_message.mac.mac, message.mac.mac);
        assert_eq!(decoded_message.meta.timestamp, message.meta.timestamp);
    }

    #[test]
    fn it_encrypts_and_decrypts_a_message_from_alice_to_bob_and_signs_and_verifies() {
        let plaintext = b"Hello, World!";
        // alice
        let alice_keypair = Ed25519Keypair::new();
        let alice_private_key = alice_keypair.get_private_key();
        let alice_public_key = alice_keypair.get_public_key();

        // bob
        let bob_keypair = Ed25519Keypair::new();
        let bob_private_key = bob_keypair.get_private_key();
        let bob_public_key = bob_keypair.get_public_key();

        let bob_secret_key_x25519: X25519PrivateKey = (&bob_private_key).into();
        let bob_public_key_x25519: X25519PublicKey = (&bob_public_key).into();

        // alice encrypts and signs
        let encryptor = encryptor::Encryptor::new(&[bob_public_key_x25519]);
        let message = encryptor.encrypt(plaintext, Some(&alice_private_key));

        // bob decrypts
        let decryptor = decryptor::Decryptor::new(&message);
        let file_key = decryptor
            .decrypt_file_key(&bob_secret_key_x25519)
            .expect("could decrypt file key");
        assert_eq!(decryptor.verify_message_mac(&file_key), true);
        let decrypted_plaintext = decryptor
            .decrypt_payload(&file_key)
            .expect("could decrypt payload");
        assert_eq!(decrypted_plaintext, plaintext);
        // verify signature
        assert!(decryptor::Decryptor::verify_signature(
            &message,
            &alice_public_key,
        ));
    }
}
