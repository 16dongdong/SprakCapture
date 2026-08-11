#![allow(non_snake_case)]

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD as base64Standard};
use prost::Message;
use prost_types::{
    DescriptorProto, FieldDescriptorProto, FileDescriptorProto, FileDescriptorSet,
    field_descriptor_proto::{Label, Type},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use proxy_backend::controlApi::createControlRouter;

#[path = "support/controlState.rs"]
mod controlState;

use controlState::newControlState;

/// 经内存路由发送 JSON 控制请求，避免协议测试启动监听器或写入真实用户目录。
async fn requestJson(
    router: axum::Router,
    method: Method,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("构建协议测试请求");
    let response = router.oneshot(request).await.expect("执行协议测试请求");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("读取协议响应");
    let value = serde_json::from_slice(&bytes).expect("解析协议 JSON 响应");
    (status, value)
}

/// 构造最小 proto3 FileDescriptorSet；真实上传路径只接受 descriptor 编译产物而不读取测试工作区文件。
fn fixtureDescriptorSetBase64() -> String {
    let descriptorSet = FileDescriptorSet {
        file: vec![FileDescriptorProto {
            name: Some("fixture.proto".to_owned()),
            package: Some("fixture".to_owned()),
            syntax: Some("proto3".to_owned()),
            message_type: vec![DescriptorProto {
                name: Some("Envelope".to_owned()),
                field: vec![FieldDescriptorProto {
                    name: Some("value".to_owned()),
                    json_name: Some("value".to_owned()),
                    number: Some(1),
                    label: Some(Label::Optional as i32),
                    r#type: Some(Type::String as i32),
                    ..FieldDescriptorProto::default()
                }],
                ..DescriptorProto::default()
            }],
            ..FileDescriptorProto::default()
        }],
    };
    let mut bytes = Vec::new();
    descriptorSet
        .encode(&mut bytes)
        .expect("编码 Protobuf 测试描述符");
    base64Standard.encode(bytes)
}

/// 验证 M6 Protobuf 和 Validate 配置由同一控制路由读写，默认在线校验关闭且不需启动数据面。
#[tokio::test]
async fn protocolConfigurationEndpointsExposeSafeDefaults() {
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let (protobufStatus, protobuf) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/tools/protobuf",
        json!({}),
    )
    .await;
    assert_eq!(protobufStatus, StatusCode::OK);
    assert_eq!(protobuf["enabled"], false);
    assert_eq!(protobuf["schemas"], json!([]));

    let (updatedStatus, updated) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/protobuf",
        json!({"enabled":true,"routes":[]}),
    )
    .await;
    assert_eq!(updatedStatus, StatusCode::OK);
    assert_eq!(updated["enabled"], true);

    let (validateStatus, validate) =
        requestJson(router, Method::GET, "/api/v1/tools/validate", json!({})).await;
    assert_eq!(validateStatus, StatusCode::OK);
    assert_eq!(validate["allowOnlineValidators"], false);
    assert!(validate["validators"].as_array().is_some_and(|validators| {
        validators
            .iter()
            .any(|validator| validator["id"] == "htmlWellFormed" && validator["enabled"] == true)
    }));
}

/// 验证描述符上传先解析 FileDescriptorSet，再允许路由引用登记的消息类型，失败不会形成半配置。
#[tokio::test]
async fn protobufDescriptorUploadRegistersTypedRoute() {
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let (uploadStatus, uploaded) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/tools/protobuf/schemas",
        json!({
            "name": "fixture",
            "defaultMessageType": "fixture.Envelope",
            "base64": fixtureDescriptorSetBase64()
        }),
    )
    .await;
    assert_eq!(uploadStatus, StatusCode::OK);
    let schemaId = uploaded["schemas"]
        .as_array()
        .and_then(|schemas| schemas.first())
        .and_then(|schema| schema["id"].as_str())
        .expect("上传后的 schema 标识")
        .to_owned();

    let (updateStatus, updated) = requestJson(
        router,
        Method::PUT,
        "/api/v1/tools/protobuf",
        json!({
            "enabled": true,
            "routes": [{
                "id": "fixture-route",
                "location": {
                    "protocol": "http",
                    "host": "fixture.example",
                    "port": "80",
                    "path": "/protobuf",
                    "query": null
                },
                "messageType": "fixture.Envelope",
                "responseMessageType": "fixture.Envelope",
                "schemaId": schemaId
            }]
        }),
    )
    .await;
    assert_eq!(updateStatus, StatusCode::OK);
    assert_eq!(updated["routes"][0]["messageType"], "fixture.Envelope");
}
