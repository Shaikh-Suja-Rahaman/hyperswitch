use actix_web::{web, HttpRequest, HttpResponse};
use api_models::blocklist as api_blocklist;
use router_env::Flow;

use crate::{
    core::{api_locking, blocklist},
    routes::AppState,
    services::{api, authentication as auth, authorization::permissions::Permission},
    types::domain,
};

#[utoipa::path(
    post,
    path = "/blocklist",
    request_body = BlocklistRequest,
    responses(
        (status = 200, description = "Fingerprint Blocked", body = BlocklistResponse),
        (status = 400, description = "Invalid Data")
    ),
    tag = "Blocklist",
    operation_id = "Block a Fingerprint",
    security(("api_key" = []))
)]
pub async fn add_entry_to_blocklist(
    state: web::Data<AppState>,
    req: HttpRequest,
    json_payload: web::Json<api_blocklist::AddToBlocklistRequest>,
) -> HttpResponse {
    let flow = Flow::AddToBlocklist;
    Box::pin(api::server_wrap(
        flow,
        state,
        &req,
        json_payload.into_inner(),
        |state, auth: auth::AuthenticationData, body, _| {
            let merchant_context = domain::MerchantContext::NormalMerchant(Box::new(
                domain::Context(auth.merchant_account, auth.key_store),
            ));
            blocklist::add_entry_to_blocklist(state, merchant_context, body)
        },
        auth::auth_type(
            &auth::HeaderAuth(auth::ApiKeyAuth {
                is_connected_allowed: false,
                is_platform_allowed: false,
            }),
            &auth::JWTAuth {
                permission: Permission::MerchantAccountWrite,
            },
            req.headers(),
        ),
        api_locking::LockAction::NotApplicable,
    ))
    .await
}

#[utoipa::path(
    delete,
    path = "/blocklist",
    request_body = BlocklistRequest,
    responses(
        (status = 200, description = "Fingerprint Unblocked", body = BlocklistResponse),
        (status = 400, description = "Invalid Data")
    ),
    tag = "Blocklist",
    operation_id = "Unblock a Fingerprint",
    security(("api_key" = []))
)]
pub async fn remove_entry_from_blocklist(
    state: web::Data<AppState>,
    req: HttpRequest,
    json_payload: web::Json<api_blocklist::DeleteFromBlocklistRequest>,
) -> HttpResponse {
    let flow = Flow::DeleteFromBlocklist;
    Box::pin(api::server_wrap(
        flow,
        state,
        &req,
        json_payload.into_inner(),
        |state, auth: auth::AuthenticationData, body, _| {
            let merchant_context = domain::MerchantContext::NormalMerchant(Box::new(
                domain::Context(auth.merchant_account, auth.key_store),
            ));
            blocklist::remove_entry_from_blocklist(state, merchant_context, body)
        },
        auth::auth_type(
            &auth::HeaderAuth(auth::ApiKeyAuth {
                is_connected_allowed: false,
                is_platform_allowed: false,
            }),
            &auth::JWTAuth {
                permission: Permission::MerchantAccountWrite,
            },
            req.headers(),
        ),
        api_locking::LockAction::NotApplicable,
    ))
    .await
}

#[utoipa::path(
    get,
    path = "/blocklist",
    params (
        ("data_kind" = BlocklistDataKind, Query, description = "Kind of the fingerprint list requested"),
    ),
    responses(
        (status = 200, description = "Blocked Fingerprints", body = BlocklistResponse),
        (status = 400, description = "Invalid Data")
    ),
    tag = "Blocklist",
    operation_id = "List Blocked fingerprints of a particular kind",
    security(("api_key" = []))
)]
pub async fn list_blocked_payment_methods(
    state: web::Data<AppState>,
    req: HttpRequest,
    query_payload: web::Query<api_blocklist::ListBlocklistQuery>,
) -> HttpResponse {
    let flow = Flow::ListBlocklist;
    Box::pin(api::server_wrap(
        flow,
        state,
        &req,
        query_payload.into_inner(),
        |state, auth: auth::AuthenticationData, query, _| {
            let merchant_context = domain::MerchantContext::NormalMerchant(Box::new(
                domain::Context(auth.merchant_account, auth.key_store),
            ));
            blocklist::list_blocklist_entries(state, merchant_context, query)
        },
        auth::auth_type(
            &auth::HeaderAuth(auth::ApiKeyAuth {
                is_connected_allowed: false,
                is_platform_allowed: false,
            }),
            &auth::JWTAuth {
                permission: Permission::MerchantAccountRead,
            },
            req.headers(),
        ),
        api_locking::LockAction::NotApplicable,
    ))
    .await
}

#[utoipa::path(
    post,
    path = "/blocklist/toggle",
    params (
        ("status" = bool, Query, description = "Boolean value to enable/disable blocklist"),
    ),
    responses(
        (status = 200, description = "Blocklist guard enabled/disabled", body = ToggleBlocklistResponse),
        (status = 400, description = "Invalid Data")
    ),
    tag = "Blocklist",
    operation_id = "Toggle blocklist guard for a particular merchant",
    security(("api_key" = []))
)]
pub async fn toggle_blocklist_guard(
    state: web::Data<AppState>,
    req: HttpRequest,
    query_payload: web::Query<api_blocklist::ToggleBlocklistQuery>,
) -> HttpResponse {
    let flow = Flow::ListBlocklist;
    Box::pin(api::server_wrap(
        flow,
        state,
        &req,
        query_payload.into_inner(),
        |state, auth: auth::AuthenticationData, query, _| {
            let merchant_context = domain::MerchantContext::NormalMerchant(Box::new(
                domain::Context(auth.merchant_account, auth.key_store),
            ));
            blocklist::toggle_blocklist_guard(state, merchant_context, query)
        },
        auth::auth_type(
            &auth::HeaderAuth(auth::ApiKeyAuth {
                is_connected_allowed: false,
                is_platform_allowed: false,
            }),
            &auth::JWTAuth {
                permission: Permission::MerchantAccountWrite,
            },
            req.headers(),
        ),
        api_locking::LockAction::NotApplicable,
    ))
    .await
}
