use crate::trusted_service_database::{
    DatabaseOperation, DatabaseRequest, TrustedServiceDatabaseContract, MAX_REQUEST_BYTES,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum GatewayStorageOperation {
    Get { key: String },
    Put { key: String, value_json: String },
    Delete { key: String },
    List { prefix: String, limit: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayRequestEnvelope {
    pub addon_id: String,
    pub activation_instance: String,
    pub capability: String,
    pub operation: GatewayStorageOperation,
}

pub fn encode_request(request: &GatewayRequestEnvelope) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(request).map_err(|_| "Invalid gateway request".to_string())?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("Gateway request is too large".to_string());
    }
    Ok(bytes)
}

pub fn decode_request(
    bytes: &[u8],
    contract: &TrustedServiceDatabaseContract,
) -> Result<DatabaseRequest, String> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("Gateway request is too large".to_string());
    }
    let envelope: GatewayRequestEnvelope =
        serde_json::from_slice(bytes).map_err(|_| "Invalid gateway request".to_string())?;
    if envelope.capability != "storage" {
        return Err("Gateway capability is not permitted".to_string());
    }
    let request = envelope.into_database_request();
    request
        .validate_against(contract)
        .map_err(|_| "Gateway request is not permitted".to_string())?;
    Ok(request)
}

impl GatewayRequestEnvelope {
    fn into_database_request(self) -> DatabaseRequest {
        let operation = match self.operation {
            GatewayStorageOperation::Get { key } => DatabaseOperation::Get { key },
            GatewayStorageOperation::Put { key, value_json } => {
                DatabaseOperation::Put { key, value_json }
            }
            GatewayStorageOperation::Delete { key } => DatabaseOperation::Delete { key },
            GatewayStorageOperation::List { prefix, limit } => {
                DatabaseOperation::List { prefix, limit }
            }
        };
        DatabaseRequest {
            addon_id: self.addon_id,
            activation_instance: self.activation_instance,
            operation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_request, encode_request, GatewayRequestEnvelope, GatewayStorageOperation};
    use crate::trusted_service_database::{TrustedServiceDatabaseContract, MAX_REQUEST_BYTES};

    fn contract() -> TrustedServiceDatabaseContract {
        TrustedServiceDatabaseContract::core_storage_api("example.weather", "activation-1")
    }

    fn request() -> GatewayRequestEnvelope {
        GatewayRequestEnvelope {
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
            capability: "storage".to_string(),
            operation: GatewayStorageOperation::Put {
                key: "state".to_string(),
                value_json: "{\"enabled\":true}".to_string(),
            },
        }
    }

    #[test]
    fn round_trips_bounded_typed_storage_request() {
        let bytes = encode_request(&request()).expect("request should encode");
        let decoded = decode_request(&bytes, &contract()).expect("request should decode");
        assert_eq!(decoded.addon_id, "example.weather");
        assert_eq!(decoded.activation_instance, "activation-1");
    }

    #[test]
    fn rejects_wrong_capability_and_identity_before_backend_access() {
        let mut wrong_capability = request();
        wrong_capability.capability = "read:clients".to_string();
        let bytes = encode_request(&wrong_capability).expect("envelope should encode");
        assert!(decode_request(&bytes, &contract()).is_err());

        let mut wrong_identity = request();
        wrong_identity.activation_instance = "activation-2".to_string();
        let bytes = encode_request(&wrong_identity).expect("envelope should encode");
        assert!(decode_request(&bytes, &contract()).is_err());
    }

    #[test]
    fn rejects_oversized_frames_without_parsing_them() {
        let bytes = vec![b' '; MAX_REQUEST_BYTES + 1];
        assert!(decode_request(&bytes, &contract()).is_err());
    }
}
