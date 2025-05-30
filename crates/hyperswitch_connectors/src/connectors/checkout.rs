pub mod transformers;
use std::sync::LazyLock;

use common_enums::{enums, CallConnectorAction, PaymentAction};
use common_utils::{
    crypto,
    errors::CustomResult,
    ext_traits::ByteSliceExt,
    request::{Method, Request, RequestBuilder, RequestContent},
    types::{AmountConvertor, MinorUnit, MinorUnitForConnector},
};
use error_stack::ResultExt;
use hyperswitch_domain_models::{
    router_data::{AccessToken, ConnectorAuthType, ErrorResponse, RouterData},
    router_flow_types::{
        access_token_auth::AccessTokenAuth,
        payments::{Authorize, Capture, PSync, PaymentMethodToken, Session, SetupMandate, Void},
        refunds::{Execute, RSync},
        Accept, Defend, Evidence, Retrieve, Upload,
    },
    router_request_types::{
        AcceptDisputeRequestData, AccessTokenRequestData, DefendDisputeRequestData,
        PaymentMethodTokenizationData, PaymentsAuthorizeData, PaymentsCancelData,
        PaymentsCaptureData, PaymentsSessionData, PaymentsSyncData, RefundsData,
        RetrieveFileRequestData, SetupMandateRequestData, SubmitEvidenceRequestData,
        SyncRequestType, UploadFileRequestData,
    },
    router_response_types::{
        AcceptDisputeResponse, ConnectorInfo, DefendDisputeResponse, PaymentMethodDetails,
        PaymentsResponseData, RefundsResponseData, RetrieveFileResponse, SubmitEvidenceResponse,
        SupportedPaymentMethods, SupportedPaymentMethodsExt, UploadFileResponse,
    },
    types::{
        PaymentsAuthorizeRouterData, PaymentsCancelRouterData, PaymentsCaptureRouterData,
        PaymentsSyncRouterData, RefundsRouterData, TokenizationRouterData,
    },
};
use hyperswitch_interfaces::{
    api::{
        self,
        disputes::{AcceptDispute, DefendDispute, Dispute, SubmitEvidence},
        files::{FilePurpose, FileUpload, RetrieveFile, UploadFile},
        CaptureSyncMethod, ConnectorCommon, ConnectorCommonExt, ConnectorIntegration,
        ConnectorSpecifications, ConnectorValidation, MandateSetup,
    },
    configs::Connectors,
    consts,
    disputes::DisputePayload,
    errors,
    events::connector_api_logs::ConnectorEvent,
    types::{
        AcceptDisputeType, DefendDisputeType, PaymentsAuthorizeType, PaymentsCaptureType,
        PaymentsSyncType, PaymentsVoidType, RefundExecuteType, RefundSyncType, Response,
        SubmitEvidenceType, TokenizationType, UploadFileType,
    },
    webhooks,
};
use masking::{Mask, Maskable, PeekInterface};
use transformers::CheckoutErrorResponse;

use self::transformers as checkout;
use crate::{
    constants::headers,
    types::{
        AcceptDisputeRouterData, DefendDisputeRouterData, ResponseRouterData,
        SubmitEvidenceRouterData, UploadFileRouterData,
    },
    utils::{self, ConnectorErrorType, RefundsRequestData},
};

#[derive(Clone)]
pub struct Checkout {
    amount_converter: &'static (dyn AmountConvertor<Output = MinorUnit> + Sync),
}

impl Checkout {
    pub fn new() -> &'static Self {
        &Self {
            amount_converter: &MinorUnitForConnector,
        }
    }
}

impl<Flow, Request, Response> ConnectorCommonExt<Flow, Request, Response> for Checkout
where
    Self: ConnectorIntegration<Flow, Request, Response>,
{
    fn build_headers(
        &self,
        req: &RouterData<Flow, Request, Response>,
        _connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        let mut header = vec![(
            headers::CONTENT_TYPE.to_string(),
            PaymentsAuthorizeType::get_content_type(self)
                .to_string()
                .into(),
        )];
        let mut api_key = self.get_auth_header(&req.connector_auth_type)?;
        header.append(&mut api_key);
        Ok(header)
    }
}

impl ConnectorCommon for Checkout {
    fn id(&self) -> &'static str {
        "checkout"
    }

    fn get_currency_unit(&self) -> api::CurrencyUnit {
        api::CurrencyUnit::Minor
    }

    fn common_get_content_type(&self) -> &'static str {
        "application/json"
    }

    fn get_auth_header(
        &self,
        auth_type: &ConnectorAuthType,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        let auth = checkout::CheckoutAuthType::try_from(auth_type)
            .change_context(errors::ConnectorError::FailedToObtainAuthType)?;
        Ok(vec![(
            headers::AUTHORIZATION.to_string(),
            format!("Bearer {}", auth.api_secret.peek()).into_masked(),
        )])
    }

    fn base_url<'a>(&self, connectors: &'a Connectors) -> &'a str {
        connectors.checkout.base_url.as_ref()
    }
    fn build_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        let response: CheckoutErrorResponse = if res.response.is_empty() {
            let (error_codes, error_type) = if res.status_code == 401 {
                (
                    Some(vec!["Invalid api key".to_string()]),
                    Some("invalid_api_key".to_string()),
                )
            } else {
                (None, None)
            };
            CheckoutErrorResponse {
                request_id: None,
                error_codes,
                error_type,
            }
        } else {
            res.response
                .parse_struct("ErrorResponse")
                .change_context(errors::ConnectorError::ResponseDeserializationFailed)?
        };

        event_builder.map(|i| i.set_error_response_body(&response));
        router_env::logger::info!(connector_response=?response);

        let errors_list = response.error_codes.clone().unwrap_or_default();
        let option_error_code_message = utils::get_error_code_error_message_based_on_priority(
            self.clone(),
            errors_list
                .into_iter()
                .map(|errors| errors.into())
                .collect(),
        );
        Ok(ErrorResponse {
            status_code: res.status_code,
            code: option_error_code_message
                .clone()
                .map(|error_code_message| error_code_message.error_code)
                .unwrap_or(consts::NO_ERROR_CODE.to_string()),
            message: option_error_code_message
                .map(|error_code_message| error_code_message.error_message)
                .unwrap_or(consts::NO_ERROR_MESSAGE.to_string()),
            reason: response
                .error_codes
                .map(|errors| errors.join(" & "))
                .or(response.error_type),
            attempt_status: None,
            connector_transaction_id: response.request_id,
            network_advice_code: None,
            network_decline_code: None,
            network_error_message: None,
        })
    }
}

impl ConnectorValidation for Checkout {
    fn validate_connector_against_payment_request(
        &self,
        capture_method: Option<enums::CaptureMethod>,
        _payment_method: enums::PaymentMethod,
        _pmt: Option<enums::PaymentMethodType>,
    ) -> CustomResult<(), errors::ConnectorError> {
        let capture_method = capture_method.unwrap_or_default();
        match capture_method {
            enums::CaptureMethod::Automatic
            | enums::CaptureMethod::SequentialAutomatic
            | enums::CaptureMethod::Manual
            | enums::CaptureMethod::ManualMultiple => Ok(()),
            enums::CaptureMethod::Scheduled => Err(utils::construct_not_implemented_error_report(
                capture_method,
                self.id(),
            )),
        }
    }
}

impl api::Payment for Checkout {}

impl api::PaymentAuthorize for Checkout {}
impl api::PaymentSync for Checkout {}
impl api::PaymentVoid for Checkout {}
impl api::PaymentCapture for Checkout {}
impl api::PaymentSession for Checkout {}
impl api::ConnectorAccessToken for Checkout {}
impl AcceptDispute for Checkout {}
impl api::PaymentToken for Checkout {}
impl Dispute for Checkout {}
impl RetrieveFile for Checkout {}
impl DefendDispute for Checkout {}

impl ConnectorIntegration<PaymentMethodToken, PaymentMethodTokenizationData, PaymentsResponseData>
    for Checkout
{
    fn get_headers(
        &self,
        req: &TokenizationRouterData,
        _connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        let mut header = vec![(
            headers::CONTENT_TYPE.to_string(),
            self.common_get_content_type().to_string().into(),
        )];
        let api_key = checkout::CheckoutAuthType::try_from(&req.connector_auth_type)
            .change_context(errors::ConnectorError::FailedToObtainAuthType)?;
        let mut auth = vec![(
            headers::AUTHORIZATION.to_string(),
            format!("Bearer {}", api_key.api_key.peek()).into_masked(),
        )];
        header.append(&mut auth);
        Ok(header)
    }

    fn get_content_type(&self) -> &'static str {
        self.common_get_content_type()
    }

    fn get_url(
        &self,
        _req: &TokenizationRouterData,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        Ok(format!("{}tokens", self.base_url(connectors)))
    }

    fn get_request_body(
        &self,
        req: &TokenizationRouterData,
        _connectors: &Connectors,
    ) -> CustomResult<RequestContent, errors::ConnectorError> {
        let connector_req = checkout::TokenRequest::try_from(req)?;
        Ok(RequestContent::Json(Box::new(connector_req)))
    }

    fn build_request(
        &self,
        req: &TokenizationRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        Ok(Some(
            RequestBuilder::new()
                .method(Method::Post)
                .url(&TokenizationType::get_url(self, req, connectors)?)
                .attach_default_headers()
                .headers(TokenizationType::get_headers(self, req, connectors)?)
                .set_body(TokenizationType::get_request_body(self, req, connectors)?)
                .build(),
        ))
    }

    fn handle_response(
        &self,
        data: &TokenizationRouterData,
        event_builder: Option<&mut ConnectorEvent>,
        res: Response,
    ) -> CustomResult<TokenizationRouterData, errors::ConnectorError>
    where
        PaymentsResponseData: Clone,
    {
        let response: checkout::CheckoutTokenResponse = res
            .response
            .parse_struct("CheckoutTokenResponse")
            .change_context(errors::ConnectorError::ResponseDeserializationFailed)?;
        event_builder.map(|i| i.set_response_body(&response));
        router_env::logger::info!(connector_response=?response);
        RouterData::try_from(ResponseRouterData {
            response,
            data: data.clone(),
            http_code: res.status_code,
        })
        .change_context(errors::ConnectorError::ResponseHandlingFailed)
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

impl ConnectorIntegration<Session, PaymentsSessionData, PaymentsResponseData> for Checkout {
    // Not Implemented (R)
}

impl ConnectorIntegration<AccessTokenAuth, AccessTokenRequestData, AccessToken> for Checkout {
    // Not Implemented (R)
}

impl MandateSetup for Checkout {}

impl ConnectorIntegration<SetupMandate, SetupMandateRequestData, PaymentsResponseData>
    for Checkout
{
    // Issue: #173
    fn build_request(
        &self,
        _req: &RouterData<SetupMandate, SetupMandateRequestData, PaymentsResponseData>,
        _connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        Err(
            errors::ConnectorError::NotImplemented("Setup Mandate flow for Checkout".to_string())
                .into(),
        )
    }
}

impl ConnectorIntegration<Capture, PaymentsCaptureData, PaymentsResponseData> for Checkout {
    fn get_headers(
        &self,
        req: &PaymentsCaptureRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        self.build_headers(req, connectors)
    }

    fn get_url(
        &self,
        req: &PaymentsCaptureRouterData,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        let id = req.request.connector_transaction_id.as_str();
        Ok(format!(
            "{}payments/{id}/captures",
            self.base_url(connectors)
        ))
    }
    fn get_request_body(
        &self,
        req: &PaymentsCaptureRouterData,
        _connectors: &Connectors,
    ) -> CustomResult<RequestContent, errors::ConnectorError> {
        let amount = utils::convert_amount(
            self.amount_converter,
            req.request.minor_amount_to_capture,
            req.request.currency,
        )?;

        let connector_router_data = checkout::CheckoutRouterData::from((amount, req));
        let connector_req = checkout::PaymentCaptureRequest::try_from(&connector_router_data)?;
        Ok(RequestContent::Json(Box::new(connector_req)))
    }

    fn build_request(
        &self,
        req: &PaymentsCaptureRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        Ok(Some(
            RequestBuilder::new()
                .method(Method::Post)
                .url(&PaymentsCaptureType::get_url(self, req, connectors)?)
                .attach_default_headers()
                .headers(PaymentsCaptureType::get_headers(self, req, connectors)?)
                .set_body(PaymentsCaptureType::get_request_body(
                    self, req, connectors,
                )?)
                .build(),
        ))
    }

    fn handle_response(
        &self,
        data: &PaymentsCaptureRouterData,
        event_builder: Option<&mut ConnectorEvent>,
        res: Response,
    ) -> CustomResult<PaymentsCaptureRouterData, errors::ConnectorError> {
        let response: checkout::PaymentCaptureResponse = res
            .response
            .parse_struct("CaptureResponse")
            .change_context(errors::ConnectorError::ResponseDeserializationFailed)?;
        event_builder.map(|i| i.set_response_body(&response));
        router_env::logger::info!(connector_response=?response);
        RouterData::try_from(ResponseRouterData {
            response,
            data: data.clone(),
            http_code: res.status_code,
        })
        .change_context(errors::ConnectorError::ResponseHandlingFailed)
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

impl ConnectorIntegration<PSync, PaymentsSyncData, PaymentsResponseData> for Checkout {
    fn get_headers(
        &self,
        req: &PaymentsSyncRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        self.build_headers(req, connectors)
    }

    fn get_url(
        &self,
        req: &PaymentsSyncRouterData,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        let suffix = match req.request.sync_type {
            SyncRequestType::MultipleCaptureSync(_) => "/actions",
            SyncRequestType::SinglePaymentSync => "",
        };
        Ok(format!(
            "{}{}{}{}",
            self.base_url(connectors),
            "payments/",
            req.request
                .connector_transaction_id
                .get_connector_transaction_id()
                .change_context(errors::ConnectorError::MissingConnectorTransactionID)?,
            suffix
        ))
    }

    fn build_request(
        &self,
        req: &PaymentsSyncRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        Ok(Some(
            RequestBuilder::new()
                .method(Method::Get)
                .url(&PaymentsSyncType::get_url(self, req, connectors)?)
                .attach_default_headers()
                .headers(PaymentsSyncType::get_headers(self, req, connectors)?)
                .build(),
        ))
    }

    fn handle_response(
        &self,
        data: &PaymentsSyncRouterData,
        event_builder: Option<&mut ConnectorEvent>,
        res: Response,
    ) -> CustomResult<PaymentsSyncRouterData, errors::ConnectorError>
    where
        PSync: Clone,
        PaymentsSyncData: Clone,
        PaymentsResponseData: Clone,
    {
        match &data.request.sync_type {
            SyncRequestType::MultipleCaptureSync(_) => {
                let response: checkout::PaymentsResponseEnum = res
                    .response
                    .parse_struct("checkout::PaymentsResponseEnum")
                    .change_context(errors::ConnectorError::ResponseDeserializationFailed)?;
                event_builder.map(|i| i.set_response_body(&response));
                router_env::logger::info!(connector_response=?response);
                RouterData::try_from(ResponseRouterData {
                    response,
                    data: data.clone(),
                    http_code: res.status_code,
                })
                .change_context(errors::ConnectorError::ResponseHandlingFailed)
            }
            SyncRequestType::SinglePaymentSync => {
                let response: checkout::PaymentsResponse = res
                    .response
                    .parse_struct("PaymentsResponse")
                    .change_context(errors::ConnectorError::ResponseDeserializationFailed)?;
                event_builder.map(|i| i.set_response_body(&response));
                router_env::logger::info!(connector_response=?response);
                RouterData::try_from(ResponseRouterData {
                    response,
                    data: data.clone(),
                    http_code: res.status_code,
                })
                .change_context(errors::ConnectorError::ResponseHandlingFailed)
            }
        }
    }

    fn get_multiple_capture_sync_method(
        &self,
    ) -> CustomResult<CaptureSyncMethod, errors::ConnectorError> {
        Ok(CaptureSyncMethod::Bulk)
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

impl ConnectorIntegration<Authorize, PaymentsAuthorizeData, PaymentsResponseData> for Checkout {
    fn get_headers(
        &self,
        req: &PaymentsAuthorizeRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        self.build_headers(req, connectors)
    }

    fn get_url(
        &self,
        _req: &PaymentsAuthorizeRouterData,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        Ok(format!("{}{}", self.base_url(connectors), "payments"))
    }

    fn get_request_body(
        &self,
        req: &PaymentsAuthorizeRouterData,
        _connectors: &Connectors,
    ) -> CustomResult<RequestContent, errors::ConnectorError> {
        let amount = utils::convert_amount(
            self.amount_converter,
            req.request.minor_amount,
            req.request.currency,
        )?;

        let connector_router_data = checkout::CheckoutRouterData::from((amount, req));
        let connector_req = checkout::PaymentsRequest::try_from(&connector_router_data)?;
        Ok(RequestContent::Json(Box::new(connector_req)))
    }
    fn build_request(
        &self,
        req: &RouterData<Authorize, PaymentsAuthorizeData, PaymentsResponseData>,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        Ok(Some(
            RequestBuilder::new()
                .method(Method::Post)
                .url(&PaymentsAuthorizeType::get_url(self, req, connectors)?)
                .attach_default_headers()
                .headers(PaymentsAuthorizeType::get_headers(self, req, connectors)?)
                .set_body(PaymentsAuthorizeType::get_request_body(
                    self, req, connectors,
                )?)
                .build(),
        ))
    }

    fn handle_response(
        &self,
        data: &PaymentsAuthorizeRouterData,
        event_builder: Option<&mut ConnectorEvent>,
        res: Response,
    ) -> CustomResult<PaymentsAuthorizeRouterData, errors::ConnectorError> {
        let response: checkout::PaymentsResponse = res
            .response
            .parse_struct("PaymentIntentResponse")
            .change_context(errors::ConnectorError::ResponseDeserializationFailed)?;
        event_builder.map(|i| i.set_response_body(&response));
        router_env::logger::info!(connector_response=?response);
        RouterData::try_from(ResponseRouterData {
            response,
            data: data.clone(),
            http_code: res.status_code,
        })
        .change_context(errors::ConnectorError::ResponseHandlingFailed)
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

impl ConnectorIntegration<Void, PaymentsCancelData, PaymentsResponseData> for Checkout {
    fn get_headers(
        &self,
        req: &PaymentsCancelRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        self.build_headers(req, connectors)
    }

    fn get_url(
        &self,
        req: &PaymentsCancelRouterData,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        Ok(format!(
            "{}payments/{}/voids",
            self.base_url(connectors),
            &req.request.connector_transaction_id
        ))
    }

    fn get_request_body(
        &self,
        req: &PaymentsCancelRouterData,
        _connectors: &Connectors,
    ) -> CustomResult<RequestContent, errors::ConnectorError> {
        let connector_req = checkout::PaymentVoidRequest::try_from(req)?;
        Ok(RequestContent::Json(Box::new(connector_req)))
    }
    fn build_request(
        &self,
        req: &PaymentsCancelRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        Ok(Some(
            RequestBuilder::new()
                .method(Method::Post)
                .url(&PaymentsVoidType::get_url(self, req, connectors)?)
                .attach_default_headers()
                .headers(PaymentsVoidType::get_headers(self, req, connectors)?)
                .set_body(PaymentsVoidType::get_request_body(self, req, connectors)?)
                .build(),
        ))
    }

    fn handle_response(
        &self,
        data: &PaymentsCancelRouterData,
        event_builder: Option<&mut ConnectorEvent>,
        res: Response,
    ) -> CustomResult<PaymentsCancelRouterData, errors::ConnectorError> {
        let mut response: checkout::PaymentVoidResponse = res
            .response
            .parse_struct("PaymentVoidResponse")
            .change_context(errors::ConnectorError::ResponseDeserializationFailed)?;
        event_builder.map(|i| i.set_response_body(&response));
        router_env::logger::info!(connector_response=?response);

        response.status = res.status_code;

        RouterData::try_from(ResponseRouterData {
            response,
            data: data.clone(),
            http_code: res.status_code,
        })
        .change_context(errors::ConnectorError::ResponseHandlingFailed)
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

impl api::Refund for Checkout {}
impl api::RefundExecute for Checkout {}
impl api::RefundSync for Checkout {}

impl ConnectorIntegration<Execute, RefundsData, RefundsResponseData> for Checkout {
    fn get_headers(
        &self,
        req: &RefundsRouterData<Execute>,
        connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        self.build_headers(req, connectors)
    }

    fn get_content_type(&self) -> &'static str {
        self.common_get_content_type()
    }

    fn get_url(
        &self,
        req: &RefundsRouterData<Execute>,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        let id = req.request.connector_transaction_id.clone();
        Ok(format!(
            "{}payments/{}/refunds",
            self.base_url(connectors),
            id
        ))
    }

    fn get_request_body(
        &self,
        req: &RefundsRouterData<Execute>,
        _connectors: &Connectors,
    ) -> CustomResult<RequestContent, errors::ConnectorError> {
        let amount = utils::convert_amount(
            self.amount_converter,
            req.request.minor_refund_amount,
            req.request.currency,
        )?;

        let connector_router_data = checkout::CheckoutRouterData::from((amount, req));
        let connector_req = checkout::RefundRequest::try_from(&connector_router_data)?;
        Ok(RequestContent::Json(Box::new(connector_req)))
    }

    fn build_request(
        &self,
        req: &RefundsRouterData<Execute>,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        let request = RequestBuilder::new()
            .method(Method::Post)
            .url(&RefundExecuteType::get_url(self, req, connectors)?)
            .attach_default_headers()
            .headers(RefundExecuteType::get_headers(self, req, connectors)?)
            .set_body(RefundExecuteType::get_request_body(self, req, connectors)?)
            .build();
        Ok(Some(request))
    }

    fn handle_response(
        &self,
        data: &RefundsRouterData<Execute>,
        event_builder: Option<&mut ConnectorEvent>,
        res: Response,
    ) -> CustomResult<RefundsRouterData<Execute>, errors::ConnectorError> {
        let response: checkout::RefundResponse = res
            .response
            .parse_struct("checkout::RefundResponse")
            .change_context(errors::ConnectorError::ResponseDeserializationFailed)?;
        event_builder.map(|i| i.set_response_body(&response));
        router_env::logger::info!(connector_response=?response);
        let response = checkout::CheckoutRefundResponse {
            response,
            status: res.status_code,
        };
        RouterData::try_from(ResponseRouterData {
            response,
            data: data.clone(),
            http_code: res.status_code,
        })
        .change_context(errors::ConnectorError::ResponseHandlingFailed)
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

impl ConnectorIntegration<RSync, RefundsData, RefundsResponseData> for Checkout {
    fn get_headers(
        &self,
        req: &RefundsRouterData<RSync>,
        connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        self.build_headers(req, connectors)
    }

    fn get_url(
        &self,
        req: &RefundsRouterData<RSync>,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        let id = req.request.connector_transaction_id.clone();
        Ok(format!(
            "{}/payments/{}/actions",
            self.base_url(connectors),
            id
        ))
    }

    fn build_request(
        &self,
        req: &RefundsRouterData<RSync>,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        Ok(Some(
            RequestBuilder::new()
                .method(Method::Get)
                .url(&RefundSyncType::get_url(self, req, connectors)?)
                .attach_default_headers()
                .headers(RefundSyncType::get_headers(self, req, connectors)?)
                .build(),
        ))
    }

    fn handle_response(
        &self,
        data: &RefundsRouterData<RSync>,
        event_builder: Option<&mut ConnectorEvent>,
        res: Response,
    ) -> CustomResult<RefundsRouterData<RSync>, errors::ConnectorError> {
        let refund_action_id = data.request.get_connector_refund_id()?;

        let response: Vec<checkout::ActionResponse> = res
            .response
            .parse_struct("checkout::CheckoutRefundResponse")
            .change_context(errors::ConnectorError::ResponseDeserializationFailed)?;

        event_builder.map(|i| i.set_response_body(&response));
        router_env::logger::info!(connector_response=?response);

        let response = response
            .iter()
            .find(|&x| x.action_id.clone() == refund_action_id)
            .ok_or(errors::ConnectorError::ResponseHandlingFailed)?;
        RouterData::try_from(ResponseRouterData {
            response,
            data: data.clone(),
            http_code: res.status_code,
        })
        .change_context(errors::ConnectorError::ResponseHandlingFailed)
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

impl ConnectorIntegration<Accept, AcceptDisputeRequestData, AcceptDisputeResponse> for Checkout {
    fn get_headers(
        &self,
        req: &AcceptDisputeRouterData,
        _connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        let mut header = vec![(
            headers::CONTENT_TYPE.to_string(),
            AcceptDisputeType::get_content_type(self).to_string().into(),
        )];
        let mut api_key = self.get_auth_header(&req.connector_auth_type)?;
        header.append(&mut api_key);
        Ok(header)
    }

    fn get_url(
        &self,
        req: &AcceptDisputeRouterData,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        Ok(format!(
            "{}{}{}{}",
            self.base_url(connectors),
            "disputes/",
            req.request.connector_dispute_id,
            "/accept"
        ))
    }

    fn build_request(
        &self,
        req: &AcceptDisputeRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        Ok(Some(
            RequestBuilder::new()
                .method(Method::Post)
                .url(&AcceptDisputeType::get_url(self, req, connectors)?)
                .attach_default_headers()
                .headers(AcceptDisputeType::get_headers(self, req, connectors)?)
                .build(),
        ))
    }

    fn handle_response(
        &self,
        data: &AcceptDisputeRouterData,
        _event_builder: Option<&mut ConnectorEvent>,
        _res: Response,
    ) -> CustomResult<AcceptDisputeRouterData, errors::ConnectorError> {
        Ok(AcceptDisputeRouterData {
            response: Ok(AcceptDisputeResponse {
                dispute_status: enums::DisputeStatus::DisputeAccepted,
                connector_status: None,
            }),
            ..data.clone()
        })
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

impl UploadFile for Checkout {}

impl ConnectorIntegration<Retrieve, RetrieveFileRequestData, RetrieveFileResponse> for Checkout {}

#[async_trait::async_trait]
impl FileUpload for Checkout {
    fn validate_file_upload(
        &self,
        purpose: FilePurpose,
        file_size: i32,
        file_type: mime::Mime,
    ) -> CustomResult<(), errors::ConnectorError> {
        match purpose {
            FilePurpose::DisputeEvidence => {
                let supported_file_types =
                    ["image/jpeg", "image/jpg", "image/png", "application/pdf"];
                // 4 Megabytes (MB)
                if file_size > 4000000 {
                    Err(errors::ConnectorError::FileValidationFailed {
                        reason: "file_size exceeded the max file size of 4MB".to_owned(),
                    })?
                }
                if !supported_file_types.contains(&file_type.to_string().as_str()) {
                    Err(errors::ConnectorError::FileValidationFailed {
                        reason: "file_type does not match JPEG, JPG, PNG, or PDF format".to_owned(),
                    })?
                }
            }
        }
        Ok(())
    }
}

impl ConnectorIntegration<Upload, UploadFileRequestData, UploadFileResponse> for Checkout {
    fn get_headers(
        &self,
        req: &RouterData<Upload, UploadFileRequestData, UploadFileResponse>,
        _connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        self.get_auth_header(&req.connector_auth_type)
    }

    fn get_content_type(&self) -> &'static str {
        "multipart/form-data"
    }

    fn get_url(
        &self,
        _req: &UploadFileRouterData,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        Ok(format!("{}{}", self.base_url(connectors), "files"))
    }

    fn get_request_body(
        &self,
        req: &UploadFileRouterData,
        _connectors: &Connectors,
    ) -> CustomResult<RequestContent, errors::ConnectorError> {
        let connector_req = transformers::construct_file_upload_request(req.clone())?;
        Ok(RequestContent::FormData(connector_req))
    }

    fn build_request(
        &self,
        req: &UploadFileRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        Ok(Some(
            RequestBuilder::new()
                .method(Method::Post)
                .url(&UploadFileType::get_url(self, req, connectors)?)
                .attach_default_headers()
                .headers(UploadFileType::get_headers(self, req, connectors)?)
                .set_body(UploadFileType::get_request_body(self, req, connectors)?)
                .build(),
        ))
    }

    fn handle_response(
        &self,
        data: &UploadFileRouterData,
        event_builder: Option<&mut ConnectorEvent>,
        res: Response,
    ) -> CustomResult<
        RouterData<Upload, UploadFileRequestData, UploadFileResponse>,
        errors::ConnectorError,
    > {
        let response: checkout::FileUploadResponse = res
            .response
            .parse_struct("Checkout FileUploadResponse")
            .change_context(errors::ConnectorError::ResponseDeserializationFailed)?;
        event_builder.map(|i| i.set_response_body(&response));
        router_env::logger::info!(connector_response=?response);
        Ok(UploadFileRouterData {
            response: Ok(UploadFileResponse {
                provider_file_id: response.file_id,
            }),
            ..data.clone()
        })
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

impl SubmitEvidence for Checkout {}

impl ConnectorIntegration<Evidence, SubmitEvidenceRequestData, SubmitEvidenceResponse>
    for Checkout
{
    fn get_headers(
        &self,
        req: &SubmitEvidenceRouterData,
        _connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        let mut header = vec![(
            headers::CONTENT_TYPE.to_string(),
            SubmitEvidenceType::get_content_type(self)
                .to_string()
                .into(),
        )];
        let mut api_key = self.get_auth_header(&req.connector_auth_type)?;
        header.append(&mut api_key);
        Ok(header)
    }

    fn get_url(
        &self,
        req: &SubmitEvidenceRouterData,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        Ok(format!(
            "{}disputes/{}/evidence",
            self.base_url(connectors),
            req.request.connector_dispute_id,
        ))
    }

    fn get_request_body(
        &self,
        req: &SubmitEvidenceRouterData,
        _connectors: &Connectors,
    ) -> CustomResult<RequestContent, errors::ConnectorError> {
        let connector_req = checkout::Evidence::try_from(req)?;
        Ok(RequestContent::Json(Box::new(connector_req)))
    }

    fn build_request(
        &self,
        req: &SubmitEvidenceRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        let request = RequestBuilder::new()
            .method(Method::Put)
            .url(&SubmitEvidenceType::get_url(self, req, connectors)?)
            .attach_default_headers()
            .headers(SubmitEvidenceType::get_headers(self, req, connectors)?)
            .set_body(SubmitEvidenceType::get_request_body(self, req, connectors)?)
            .build();
        Ok(Some(request))
    }

    fn handle_response(
        &self,
        data: &SubmitEvidenceRouterData,
        _event_builder: Option<&mut ConnectorEvent>,
        _res: Response,
    ) -> CustomResult<SubmitEvidenceRouterData, errors::ConnectorError> {
        Ok(SubmitEvidenceRouterData {
            response: Ok(SubmitEvidenceResponse {
                dispute_status: api_models::enums::DisputeStatus::DisputeChallenged,
                connector_status: None,
            }),
            ..data.clone()
        })
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

impl ConnectorIntegration<Defend, DefendDisputeRequestData, DefendDisputeResponse> for Checkout {
    fn get_headers(
        &self,
        req: &DefendDisputeRouterData,
        _connectors: &Connectors,
    ) -> CustomResult<Vec<(String, Maskable<String>)>, errors::ConnectorError> {
        let mut header = vec![(
            headers::CONTENT_TYPE.to_string(),
            DefendDisputeType::get_content_type(self).to_string().into(),
        )];
        let mut api_key = self.get_auth_header(&req.connector_auth_type)?;
        header.append(&mut api_key);
        Ok(header)
    }

    fn get_url(
        &self,
        req: &DefendDisputeRouterData,
        connectors: &Connectors,
    ) -> CustomResult<String, errors::ConnectorError> {
        Ok(format!(
            "{}disputes/{}/evidence",
            self.base_url(connectors),
            req.request.connector_dispute_id,
        ))
    }

    fn build_request(
        &self,
        req: &DefendDisputeRouterData,
        connectors: &Connectors,
    ) -> CustomResult<Option<Request>, errors::ConnectorError> {
        Ok(Some(
            RequestBuilder::new()
                .method(Method::Post)
                .url(&DefendDisputeType::get_url(self, req, connectors)?)
                .attach_default_headers()
                .headers(DefendDisputeType::get_headers(self, req, connectors)?)
                .build(),
        ))
    }

    fn handle_response(
        &self,
        data: &DefendDisputeRouterData,
        _event_builder: Option<&mut ConnectorEvent>,
        _res: Response,
    ) -> CustomResult<DefendDisputeRouterData, errors::ConnectorError> {
        Ok(DefendDisputeRouterData {
            response: Ok(DefendDisputeResponse {
                dispute_status: enums::DisputeStatus::DisputeChallenged,
                connector_status: None,
            }),
            ..data.clone()
        })
    }

    fn get_error_response(
        &self,
        res: Response,
        event_builder: Option<&mut ConnectorEvent>,
    ) -> CustomResult<ErrorResponse, errors::ConnectorError> {
        self.build_error_response(res, event_builder)
    }
}

#[async_trait::async_trait]
impl webhooks::IncomingWebhook for Checkout {
    fn get_webhook_source_verification_algorithm(
        &self,
        _request: &webhooks::IncomingWebhookRequestDetails<'_>,
    ) -> CustomResult<Box<dyn crypto::VerifySignature + Send>, errors::ConnectorError> {
        Ok(Box::new(crypto::HmacSha256))
    }
    fn get_webhook_source_verification_signature(
        &self,
        request: &webhooks::IncomingWebhookRequestDetails<'_>,
        _connector_webhook_secrets: &api_models::webhooks::ConnectorWebhookSecrets,
    ) -> CustomResult<Vec<u8>, errors::ConnectorError> {
        let signature = utils::get_header_key_value("cko-signature", request.headers)
            .change_context(errors::ConnectorError::WebhookSignatureNotFound)?;
        hex::decode(signature).change_context(errors::ConnectorError::WebhookSignatureNotFound)
    }
    fn get_webhook_source_verification_message(
        &self,
        request: &webhooks::IncomingWebhookRequestDetails<'_>,
        _merchant_id: &common_utils::id_type::MerchantId,
        _connector_webhook_secrets: &api_models::webhooks::ConnectorWebhookSecrets,
    ) -> CustomResult<Vec<u8>, errors::ConnectorError> {
        Ok(format!("{}", String::from_utf8_lossy(request.body)).into_bytes())
    }
    fn get_webhook_object_reference_id(
        &self,
        request: &webhooks::IncomingWebhookRequestDetails<'_>,
    ) -> CustomResult<api_models::webhooks::ObjectReferenceId, errors::ConnectorError> {
        let details: checkout::CheckoutWebhookBody = request
            .body
            .parse_struct("CheckoutWebhookBody")
            .change_context(errors::ConnectorError::WebhookReferenceIdNotFound)?;
        let ref_id: api_models::webhooks::ObjectReferenceId =
            if checkout::is_chargeback_event(&details.transaction_type) {
                let reference = match details.data.reference {
                    Some(reference) => {
                        api_models::payments::PaymentIdType::PaymentAttemptId(reference)
                    }
                    None => api_models::payments::PaymentIdType::ConnectorTransactionId(
                        details
                            .data
                            .payment_id
                            .ok_or(errors::ConnectorError::WebhookReferenceIdNotFound)?,
                    ),
                };
                api_models::webhooks::ObjectReferenceId::PaymentId(reference)
            } else if checkout::is_refund_event(&details.transaction_type) {
                let refund_reference = match details.data.reference {
                    Some(reference) => api_models::webhooks::RefundIdType::RefundId(reference),
                    None => api_models::webhooks::RefundIdType::ConnectorRefundId(
                        details
                            .data
                            .action_id
                            .ok_or(errors::ConnectorError::WebhookReferenceIdNotFound)?,
                    ),
                };
                api_models::webhooks::ObjectReferenceId::RefundId(refund_reference)
            } else {
                let reference_id = match details.data.reference {
                    Some(reference) => {
                        api_models::payments::PaymentIdType::PaymentAttemptId(reference)
                    }
                    None => {
                        api_models::payments::PaymentIdType::ConnectorTransactionId(details.data.id)
                    }
                };
                api_models::webhooks::ObjectReferenceId::PaymentId(reference_id)
            };
        Ok(ref_id)
    }

    fn get_webhook_event_type(
        &self,
        request: &webhooks::IncomingWebhookRequestDetails<'_>,
    ) -> CustomResult<api_models::webhooks::IncomingWebhookEvent, errors::ConnectorError> {
        let details: checkout::CheckoutWebhookEventTypeBody = request
            .body
            .parse_struct("CheckoutWebhookBody")
            .change_context(errors::ConnectorError::WebhookEventTypeNotFound)?;

        Ok(api_models::webhooks::IncomingWebhookEvent::from(
            details.transaction_type,
        ))
    }

    fn get_webhook_resource_object(
        &self,
        request: &webhooks::IncomingWebhookRequestDetails<'_>,
    ) -> CustomResult<Box<dyn masking::ErasedMaskSerialize>, errors::ConnectorError> {
        let event_type_data: checkout::CheckoutWebhookEventTypeBody = request
            .body
            .parse_struct("CheckoutWebhookBody")
            .change_context(errors::ConnectorError::WebhookBodyDecodingFailed)?;

        if checkout::is_chargeback_event(&event_type_data.transaction_type) {
            let dispute_webhook_body: checkout::CheckoutDisputeWebhookBody = request
                .body
                .parse_struct("CheckoutDisputeWebhookBody")
                .change_context(errors::ConnectorError::WebhookBodyDecodingFailed)?;
            Ok(Box::new(dispute_webhook_body.data))
        } else if checkout::is_refund_event(&event_type_data.transaction_type) {
            Ok(Box::new(checkout::RefundResponse::try_from(request)?))
        } else {
            Ok(Box::new(checkout::PaymentsResponse::try_from(request)?))
        }
    }

    fn get_dispute_details(
        &self,
        request: &webhooks::IncomingWebhookRequestDetails<'_>,
    ) -> CustomResult<DisputePayload, errors::ConnectorError> {
        let dispute_details: checkout::CheckoutDisputeWebhookBody = request
            .body
            .parse_struct("CheckoutWebhookBody")
            .change_context(errors::ConnectorError::WebhookBodyDecodingFailed)?;
        Ok(DisputePayload {
            amount: dispute_details.data.amount.to_string(),
            currency: dispute_details.data.currency,
            dispute_stage: api_models::enums::DisputeStage::from(
                dispute_details.transaction_type.clone(),
            ),
            connector_dispute_id: dispute_details.data.id,
            connector_reason: None,
            connector_reason_code: dispute_details.data.reason_code,
            challenge_required_by: dispute_details.data.evidence_required_by,
            connector_status: dispute_details.transaction_type.to_string(),
            created_at: dispute_details.created_on,
            updated_at: dispute_details.data.date,
        })
    }
}

impl api::ConnectorRedirectResponse for Checkout {
    fn get_flow_type(
        &self,
        _query_params: &str,
        _json_payload: Option<serde_json::Value>,
        action: PaymentAction,
    ) -> CustomResult<CallConnectorAction, errors::ConnectorError> {
        match action {
            PaymentAction::PSync
            | PaymentAction::CompleteAuthorize
            | PaymentAction::PaymentAuthenticateCompleteAuthorize => {
                Ok(CallConnectorAction::Trigger)
            }
        }
    }
}

impl utils::ConnectorErrorTypeMapping for Checkout {
    fn get_connector_error_type(
        &self,
        error_code: String,
        _error_message: String,
    ) -> ConnectorErrorType {
        match error_code.as_str() {
            "action_failure_limit_exceeded" => ConnectorErrorType::BusinessError,
            "address_invalid" => ConnectorErrorType::UserError,
            "amount_exceeds_balance" => ConnectorErrorType::BusinessError,
            "amount_invalid" => ConnectorErrorType::UserError,
            "api_calls_quota_exceeded" => ConnectorErrorType::TechnicalError,
            "billing_descriptor_city_invalid" => ConnectorErrorType::UserError,
            "billing_descriptor_city_required" => ConnectorErrorType::UserError,
            "billing_descriptor_name_invalid" => ConnectorErrorType::UserError,
            "billing_descriptor_name_required" => ConnectorErrorType::UserError,
            "business_invalid" => ConnectorErrorType::BusinessError,
            "business_settings_missing" => ConnectorErrorType::BusinessError,
            "capture_value_greater_than_authorized" => ConnectorErrorType::BusinessError,
            "capture_value_greater_than_remaining_authorized" => ConnectorErrorType::BusinessError,
            "card_authorization_failed" => ConnectorErrorType::UserError,
            "card_disabled" => ConnectorErrorType::UserError,
            "card_expired" => ConnectorErrorType::UserError,
            "card_expiry_month_invalid" => ConnectorErrorType::UserError,
            "card_expiry_month_required" => ConnectorErrorType::UserError,
            "card_expiry_year_invalid" => ConnectorErrorType::UserError,
            "card_expiry_year_required" => ConnectorErrorType::UserError,
            "card_holder_invalid" => ConnectorErrorType::UserError,
            "card_not_found" => ConnectorErrorType::UserError,
            "card_number_invalid" => ConnectorErrorType::UserError,
            "card_number_required" => ConnectorErrorType::UserError,
            "channel_details_invalid" => ConnectorErrorType::BusinessError,
            "channel_url_missing" => ConnectorErrorType::BusinessError,
            "charge_details_invalid" => ConnectorErrorType::BusinessError,
            "city_invalid" => ConnectorErrorType::BusinessError,
            "country_address_invalid" => ConnectorErrorType::UserError,
            "country_invalid" => ConnectorErrorType::UserError,
            "country_phone_code_invalid" => ConnectorErrorType::UserError,
            "country_phone_code_length_invalid" => ConnectorErrorType::UserError,
            "currency_invalid" => ConnectorErrorType::UserError,
            "currency_required" => ConnectorErrorType::UserError,
            "customer_already_exists" => ConnectorErrorType::BusinessError,
            "customer_email_invalid" => ConnectorErrorType::UserError,
            "customer_id_invalid" => ConnectorErrorType::BusinessError,
            "customer_not_found" => ConnectorErrorType::BusinessError,
            "customer_number_invalid" => ConnectorErrorType::UserError,
            "customer_plan_edit_failed" => ConnectorErrorType::BusinessError,
            "customer_plan_id_invalid" => ConnectorErrorType::BusinessError,
            "cvv_invalid" => ConnectorErrorType::UserError,
            "email_in_use" => ConnectorErrorType::BusinessError,
            "email_invalid" => ConnectorErrorType::UserError,
            "email_required" => ConnectorErrorType::UserError,
            "endpoint_invalid" => ConnectorErrorType::TechnicalError,
            "expiry_date_format_invalid" => ConnectorErrorType::UserError,
            "fail_url_invalid" => ConnectorErrorType::TechnicalError,
            "first_name_required" => ConnectorErrorType::UserError,
            "last_name_required" => ConnectorErrorType::UserError,
            "ip_address_invalid" => ConnectorErrorType::UserError,
            "issuer_network_unavailable" => ConnectorErrorType::TechnicalError,
            "metadata_key_invalid" => ConnectorErrorType::BusinessError,
            "parameter_invalid" => ConnectorErrorType::UserError,
            "password_invalid" => ConnectorErrorType::UserError,
            "payment_expired" => ConnectorErrorType::BusinessError,
            "payment_invalid" => ConnectorErrorType::BusinessError,
            "payment_method_invalid" => ConnectorErrorType::UserError,
            "payment_source_required" => ConnectorErrorType::UserError,
            "payment_type_invalid" => ConnectorErrorType::UserError,
            "phone_number_invalid" => ConnectorErrorType::UserError,
            "phone_number_length_invalid" => ConnectorErrorType::UserError,
            "previous_payment_id_invalid" => ConnectorErrorType::BusinessError,
            "recipient_account_number_invalid" => ConnectorErrorType::BusinessError,
            "recipient_account_number_required" => ConnectorErrorType::UserError,
            "recipient_dob_required" => ConnectorErrorType::UserError,
            "recipient_last_name_required" => ConnectorErrorType::UserError,
            "recipient_zip_invalid" => ConnectorErrorType::UserError,
            "recipient_zip_required" => ConnectorErrorType::UserError,
            "recurring_plan_exists" => ConnectorErrorType::BusinessError,
            "recurring_plan_not_exist" => ConnectorErrorType::BusinessError,
            "recurring_plan_removal_failed" => ConnectorErrorType::BusinessError,
            "request_invalid" => ConnectorErrorType::UserError,
            "request_json_invalid" => ConnectorErrorType::UserError,
            "risk_enabled_required" => ConnectorErrorType::BusinessError,
            "server_api_not_allowed" => ConnectorErrorType::TechnicalError,
            "source_email_invalid" => ConnectorErrorType::UserError,
            "source_email_required" => ConnectorErrorType::UserError,
            "source_id_invalid" => ConnectorErrorType::BusinessError,
            "source_id_or_email_required" => ConnectorErrorType::UserError,
            "source_id_required" => ConnectorErrorType::UserError,
            "source_id_unknown" => ConnectorErrorType::BusinessError,
            "source_invalid" => ConnectorErrorType::BusinessError,
            "source_or_destination_required" => ConnectorErrorType::BusinessError,
            "source_token_invalid" => ConnectorErrorType::BusinessError,
            "source_token_required" => ConnectorErrorType::UserError,
            "source_token_type_required" => ConnectorErrorType::UserError,
            "source_token_type_invalid" => ConnectorErrorType::BusinessError,
            "source_type_required" => ConnectorErrorType::UserError,
            "sub_entities_count_invalid" => ConnectorErrorType::BusinessError,
            "success_url_invalid" => ConnectorErrorType::BusinessError,
            "3ds_malfunction" => ConnectorErrorType::TechnicalError,
            "3ds_not_configured" => ConnectorErrorType::BusinessError,
            "3ds_not_enabled_for_card" => ConnectorErrorType::BusinessError,
            "3ds_not_supported" => ConnectorErrorType::BusinessError,
            "3ds_payment_required" => ConnectorErrorType::BusinessError,
            "token_expired" => ConnectorErrorType::BusinessError,
            "token_in_use" => ConnectorErrorType::BusinessError,
            "token_invalid" => ConnectorErrorType::BusinessError,
            "token_required" => ConnectorErrorType::UserError,
            "token_type_required" => ConnectorErrorType::UserError,
            "token_used" => ConnectorErrorType::BusinessError,
            "void_amount_invalid" => ConnectorErrorType::BusinessError,
            "wallet_id_invalid" => ConnectorErrorType::BusinessError,
            "zip_invalid" => ConnectorErrorType::UserError,
            "processing_key_required" => ConnectorErrorType::BusinessError,
            "processing_value_required" => ConnectorErrorType::BusinessError,
            "3ds_version_invalid" => ConnectorErrorType::BusinessError,
            "3ds_version_not_supported" => ConnectorErrorType::BusinessError,
            "processing_error" => ConnectorErrorType::TechnicalError,
            "service_unavailable" => ConnectorErrorType::TechnicalError,
            "token_type_invalid" => ConnectorErrorType::UserError,
            "token_data_invalid" => ConnectorErrorType::UserError,
            _ => ConnectorErrorType::UnknownError,
        }
    }
}

static CHECKOUT_SUPPORTED_PAYMENT_METHODS: LazyLock<SupportedPaymentMethods> =
    LazyLock::new(|| {
        let supported_capture_methods = vec![
            enums::CaptureMethod::Automatic,
            enums::CaptureMethod::Manual,
            enums::CaptureMethod::SequentialAutomatic,
            enums::CaptureMethod::ManualMultiple,
        ];

        let supported_card_network = vec![
            common_enums::CardNetwork::AmericanExpress,
            common_enums::CardNetwork::CartesBancaires,
            common_enums::CardNetwork::DinersClub,
            common_enums::CardNetwork::Discover,
            common_enums::CardNetwork::JCB,
            common_enums::CardNetwork::Mastercard,
            common_enums::CardNetwork::Visa,
            common_enums::CardNetwork::UnionPay,
        ];

        let mut checkout_supported_payment_methods = SupportedPaymentMethods::new();

        checkout_supported_payment_methods.add(
            enums::PaymentMethod::Card,
            enums::PaymentMethodType::Credit,
            PaymentMethodDetails {
                mandates: enums::FeatureStatus::NotSupported,
                refunds: enums::FeatureStatus::Supported,
                supported_capture_methods: supported_capture_methods.clone(),
                specific_features: Some(
                    api_models::feature_matrix::PaymentMethodSpecificFeatures::Card({
                        api_models::feature_matrix::CardSpecificFeatures {
                            three_ds: common_enums::FeatureStatus::Supported,
                            no_three_ds: common_enums::FeatureStatus::Supported,
                            supported_card_networks: supported_card_network.clone(),
                        }
                    }),
                ),
            },
        );

        checkout_supported_payment_methods.add(
            enums::PaymentMethod::Card,
            enums::PaymentMethodType::Debit,
            PaymentMethodDetails {
                mandates: enums::FeatureStatus::NotSupported,
                refunds: enums::FeatureStatus::Supported,
                supported_capture_methods: supported_capture_methods.clone(),
                specific_features: Some(
                    api_models::feature_matrix::PaymentMethodSpecificFeatures::Card({
                        api_models::feature_matrix::CardSpecificFeatures {
                            three_ds: common_enums::FeatureStatus::Supported,
                            no_three_ds: common_enums::FeatureStatus::Supported,
                            supported_card_networks: supported_card_network.clone(),
                        }
                    }),
                ),
            },
        );

        checkout_supported_payment_methods.add(
            enums::PaymentMethod::Wallet,
            enums::PaymentMethodType::GooglePay,
            PaymentMethodDetails {
                mandates: enums::FeatureStatus::NotSupported,
                refunds: enums::FeatureStatus::Supported,
                supported_capture_methods: supported_capture_methods.clone(),
                specific_features: None,
            },
        );

        checkout_supported_payment_methods.add(
            enums::PaymentMethod::Wallet,
            enums::PaymentMethodType::ApplePay,
            PaymentMethodDetails {
                mandates: enums::FeatureStatus::NotSupported,
                refunds: enums::FeatureStatus::Supported,
                supported_capture_methods: supported_capture_methods.clone(),
                specific_features: None,
            },
        );

        checkout_supported_payment_methods
    });

static CHECKOUT_CONNECTOR_INFO: ConnectorInfo = ConnectorInfo {
        display_name: "Checkout",
        description:
            "Checkout.com is a British multinational financial technology company that processes payments for other companies.",
        connector_type: enums::PaymentConnectorCategory::PaymentGateway,
    };

static CHECKOUT_SUPPORTED_WEBHOOK_FLOWS: [enums::EventClass; 3] = [
    enums::EventClass::Payments,
    enums::EventClass::Refunds,
    enums::EventClass::Disputes,
];

impl ConnectorSpecifications for Checkout {
    fn get_connector_about(&self) -> Option<&'static ConnectorInfo> {
        Some(&CHECKOUT_CONNECTOR_INFO)
    }

    fn get_supported_payment_methods(&self) -> Option<&'static SupportedPaymentMethods> {
        Some(&*CHECKOUT_SUPPORTED_PAYMENT_METHODS)
    }

    fn get_supported_webhook_flows(&self) -> Option<&'static [enums::EventClass]> {
        Some(&CHECKOUT_SUPPORTED_WEBHOOK_FLOWS)
    }
}
