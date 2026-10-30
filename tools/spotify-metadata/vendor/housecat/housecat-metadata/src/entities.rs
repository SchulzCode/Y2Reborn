//! Batched extended-metadata requests (spec section 6.2).
//!
//! One request carries every URI of one kind; the response carries one entry per URI with
//! a status code and an `Any` whose value is the entity's bytes.

use std::collections::HashMap;

use prost::Message;

use housecat_core::spclient::SpClient;
use housecat_core::{consts, Error, Result, SpotifyId};
use housecat_protocol::spotify::extendedmetadata as ext;

pub(crate) const PATH: &str = "/extended-metadata/v0/extended-metadata";

pub(crate) fn build_request(uris: &[String], kind: ext::ExtensionKind) -> ext::BatchedEntityRequest {
    ext::BatchedEntityRequest {
        header: None,
        entity_request: uris
            .iter()
            .map(|uri| ext::EntityRequest {
                entity_uri: uri.clone(),
                query: vec![ext::ExtensionQuery { extension_kind: kind as i32, etag: String::new() }],
            })
            .collect(),
    }
}

/// One payload per requested URI, in request order; `None` for a 404 or an absent entity.
pub(crate) fn extract(response: ext::BatchedExtensionResponse, uris: &[String]) -> Result<Vec<Option<Vec<u8>>>> {
    let mut found: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    for array in response.extended_metadata {
        for data in array.extension_data {
            let status = data.header.as_ref().map(|h| h.status_code).unwrap_or(200);
            let payload = match (status, data.extension_data) {
                (200, Some(any)) => Some(any.value),
                (200, None) | (404, _) => None,
                (other, _) => {
                    return Err(Error::protocol("extended-metadata", format!("status {other} for {}", data.entity_uri)));
                }
            };
            found.insert(data.entity_uri, payload);
        }
    }
    Ok(uris.iter().map(|uri| found.remove(uri).flatten()).collect())
}

/// One payload per id, in order; ids are sent in batches of `METADATA_BATCH_SIZE`.
pub(crate) async fn fetch(spclient: &SpClient, ids: &[SpotifyId], kind: ext::ExtensionKind) -> Result<Vec<Option<Vec<u8>>>> {
    let mut payloads = Vec::with_capacity(ids.len());
    for batch in ids.chunks(consts::METADATA_BATCH_SIZE.max(1)) {
        let uris: Vec<String> = batch.iter().map(SpotifyId::to_uri).collect();
        let bytes = spclient.post_protobuf(PATH, build_request(&uris, kind).encode_to_vec()).await?;
        let response = ext::BatchedExtensionResponse::decode(bytes)
            .map_err(|e| Error::protocol("extended-metadata", format!("bad protobuf: {e}")))?;
        payloads.extend(extract(response, &uris)?);
    }
    Ok(payloads)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(entries: Vec<(&str, i32, Option<Vec<u8>>)>) -> ext::BatchedExtensionResponse {
        ext::BatchedExtensionResponse {
            header: None,
            extended_metadata: vec![ext::EntityExtensionDataArray {
                header: None,
                extension_kind: ext::ExtensionKind::TrackV4 as i32,
                extension_data: entries
                    .into_iter()
                    .map(|(uri, status, payload)| ext::EntityExtensionData {
                        header: Some(ext::EntityExtensionDataHeader {
                            status_code: status,
                            etag: String::new(),
                            locale: String::new(),
                            cache_ttl_in_seconds: 0,
                            offline_ttl_in_seconds: 0,
                        }),
                        entity_uri: uri.to_string(),
                        extension_data: payload.map(|value| prost_types::Any { type_url: String::new(), value }),
                    })
                    .collect(),
            }],
        }
    }

    #[test]
    fn request_lists_every_uri_with_the_kind() {
        let uris = vec!["spotify:track:a".to_string(), "spotify:track:b".to_string()];
        let request = build_request(&uris, ext::ExtensionKind::TrackV4);
        assert!(request.header.is_none());
        assert_eq!(request.entity_request.len(), 2);
        assert_eq!(request.entity_request[1].entity_uri, "spotify:track:b");
        assert_eq!(request.entity_request[0].query[0].extension_kind, 10);
    }

    #[test]
    fn extract_keeps_request_order_and_maps_404_to_none() {
        let uris = vec!["spotify:track:a".to_string(), "spotify:track:b".to_string(), "spotify:track:c".to_string()];
        let response = response(vec![("spotify:track:b", 404, None), ("spotify:track:a", 200, Some(vec![1, 2]))]);
        let payloads = extract(response, &uris).unwrap();
        assert_eq!(payloads, vec![Some(vec![1, 2]), None, None]);
    }

    #[test]
    fn extract_rejects_other_statuses() {
        let uris = vec!["spotify:track:a".to_string()];
        let response = response(vec![("spotify:track:a", 500, None)]);
        assert!(matches!(extract(response, &uris), Err(Error::Protocol { .. })));
    }

    #[tokio::test]
    async fn fetch_splits_ids_into_batches_in_order() {
        use crate::client::tests::{server_with_auth, spclient};
        use housecat_core::IdKind;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server: MockServer = server_with_auth().await;
        // Every request is answered with a 404 per requested uri, so the answer depends on
        // the request and the batching is visible in the received bodies.
        struct Echo;
        impl wiremock::Respond for Echo {
            fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
                let req = ext::BatchedEntityRequest::decode(&request.body[..]).unwrap();
                let entries: Vec<(&str, i32, Option<Vec<u8>>)> =
                    req.entity_request.iter().map(|e| (e.entity_uri.as_str(), 404, None)).collect();
                ResponseTemplate::new(200).set_body_bytes(response(entries).encode_to_vec())
            }
        }
        Mock::given(method("POST")).and(path(PATH)).respond_with(Echo).mount(&server).await;

        let ids: Vec<SpotifyId> = (0u8..250).map(|i| SpotifyId::from_raw(IdKind::Track, &[i; 16]).unwrap()).collect();
        let payloads = fetch(&spclient(&server), &ids, ext::ExtensionKind::TrackV4).await.unwrap();
        assert_eq!(payloads.len(), 250);
        let sizes: Vec<usize> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == PATH)
            .map(|r| ext::BatchedEntityRequest::decode(&r.body[..]).unwrap().entity_request.len())
            .collect();
        assert_eq!(sizes, vec![100, 100, 50]);
    }
}
