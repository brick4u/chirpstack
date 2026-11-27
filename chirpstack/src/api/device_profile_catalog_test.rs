use super::*;
use crate::api::auth::{AuthID, validator::RequestValidator};
use crate::storage::{api_key, tenant, user};
use crate::test;
use tonic::Code;

fn request<T>(auth: &AuthID, value: T) -> Request<T> {
    let mut req = Request::new(value);
    req.extensions_mut().insert(auth.clone());
    req
}

async fn admin() -> AuthID {
    let user = user::create(user::User {
        email: "catalog-admin@example.com".into(),
        is_active: true,
        is_admin: true,
        ..Default::default()
    })
    .await
    .unwrap();
    AuthID::User(*user.id)
}

async fn seed() -> (device_profile::Device, device_profile::DeviceProfile) {
    let vendor = device_profile::create_vendor(device_profile::Vendor {
        name: "Sontex".into(),
        vendor_id: 123,
        ..Default::default()
    })
    .await
    .unwrap();
    let model = device_profile::create_device(device_profile::Device {
        name: "SA2-L".into(),
        vendor_id: vendor.id,
        ..Default::default()
    })
    .await
    .unwrap();
    let profile = device_profile::create(device_profile::DeviceProfile {
        name: "SA2-L EU868".into(),
        payload_codec_script: "function decodeUplink(input) { return {data: {}}; }".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    (model, profile)
}

fn link(
    profile: &device_profile::DeviceProfile,
    model: &device_profile::Device,
) -> api::SetDeviceProfileRepositoryLinkRequest {
    api::SetDeviceProfileRepositoryLinkRequest {
        id: profile.id.to_string(),
        device_id: model.id.to_string(),
        firmware_version: "1.2.3".into(),
        vendor_profile_id: 42,
    }
}

#[tokio::test]
async fn catalog_crud_and_repository_link() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(RequestValidator::new());
    let mut vendor = api::DeviceProfileVendor {
        name: "Sontex".into(),
        vendor_id: 123,
        ouis: vec!["001122".into()],
        metadata: [("website".into(), "https://example.com".into())].into(),
        ..Default::default()
    };
    vendor.id = service
        .create_vendor(request(
            &auth,
            api::CreateDeviceProfileVendorRequest {
                vendor: Some(vendor.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .id;
    let before = device_profile::get_vendor(Uuid::from_str(&vendor.id).unwrap())
        .await
        .unwrap();
    vendor.name = "Sontex updated".into();
    service
        .update_vendor(request(
            &auth,
            api::UpdateDeviceProfileVendorRequest {
                vendor: Some(vendor.clone()),
            },
        ))
        .await
        .unwrap();
    let got = service
        .get_vendor(request(
            &auth,
            api::GetDeviceProfileVendorRequest {
                id: vendor.id.clone(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(Some(vendor.clone()), got.vendor);
    assert_eq!(
        Some(helpers::datetime_to_prost_timestamp(&before.created_at)),
        got.created_at
    );

    let mut model = api::DeviceProfileDevice {
        vendor_id: vendor.id.clone(),
        name: "SA2-L".into(),
        description: "Smoke detector".into(),
        metadata: [("source".into(), "manual".into())].into(),
        ..Default::default()
    };
    model.id = service
        .create_device(request(
            &auth,
            api::CreateDeviceProfileDeviceRequest {
                device: Some(model.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .id;
    let second_vendor = service
        .create_vendor(request(
            &auth,
            api::CreateDeviceProfileVendorRequest {
                vendor: Some(api::DeviceProfileVendor {
                    name: "Other vendor".into(),
                    vendor_id: 456,
                    ..Default::default()
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .id;
    model.vendor_id = second_vendor;
    model.name = "SA2-L updated".into();
    model.description = "Description distinct from model name".into();
    service
        .update_device(request(
            &auth,
            api::UpdateDeviceProfileDeviceRequest {
                device: Some(model.clone()),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        Some(model.clone()),
        service
            .get_device(request(
                &auth,
                api::GetDeviceProfileDeviceRequest {
                    id: model.id.clone()
                }
            ))
            .await
            .unwrap()
            .into_inner()
            .device
    );

    let id = service
        .create(request(
            &auth,
            api::CreateDeviceProfileRequest {
                device_profile: Some(api::DeviceProfile {
                    name: "SA2-L global".into(),
                    adr_algorithm_id: "default".into(),
                    payload_codec_script: "codec remains untouched".into(),
                    ..Default::default()
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .id;
    let profile_id = Uuid::from_str(&id).unwrap();
    let before = device_profile::get(&profile_id).await.unwrap();
    let req = api::SetDeviceProfileRepositoryLinkRequest {
        id: id.clone(),
        device_id: model.id.clone(),
        firmware_version: "1.2.3".into(),
        vendor_profile_id: 42,
    };
    // Repeating assignment changes the same profile; it creates no catalog entries.
    for _ in 0..2 {
        service
            .set_repository_link(request(&auth, req.clone()))
            .await
            .unwrap();
    }
    let got = service
        .get(request(
            &auth,
            api::GetDeviceProfileRequest { id: id.clone() },
        ))
        .await
        .unwrap()
        .into_inner()
        .device_profile
        .unwrap();
    assert_eq!(model.id, got.device_id);
    assert_eq!("1.2.3", got.firmware_version);
    assert_eq!(
        before.id,
        device_profile::get_by_profile_id(456, 42).await.unwrap().id
    );
    assert_eq!(
        before.id,
        device_profile::get_for_device_id_region_and_fw(
            Uuid::from_str(&model.id).unwrap(),
            before.region,
            "1.2.3"
        )
        .await
        .unwrap()
        .id
    );
    let listed = service
        .list(request(
            &auth,
            api::ListDeviceProfilesRequest {
                global_only: true,
                device_id: model.id.clone(),
                limit: 10,
                ..Default::default()
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(1, listed.total_count);
    assert_eq!(id, listed.result[0].id);
    service
        .set_repository_link(request(
            &auth,
            api::SetDeviceProfileRepositoryLinkRequest {
                id,
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    let cleared = device_profile::get(&profile_id).await.unwrap();
    assert_eq!(None, cleared.device_id);
    assert!(cleared.firmware_version.is_empty());
    assert_eq!(0, cleared.vendor_profile_id);
    assert_eq!(before.payload_codec_script, cleared.payload_codec_script);
    assert_eq!(before.region, cleared.region);
    assert_eq!(before.tags, cleared.tags);
    assert_eq!(
        1,
        device_profile::get_device_count(&device_profile::DeviceFilters::default())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn catalog_link_validation_and_tenant_rejection() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(RequestValidator::new());
    let (model, profile) = seed().await;
    let valid = link(&profile, &model);
    let invalid = [
        (
            api::SetDeviceProfileRepositoryLinkRequest {
                device_id: "bad-id".into(),
                ..valid.clone()
            },
            Code::InvalidArgument,
        ),
        (
            api::SetDeviceProfileRepositoryLinkRequest {
                device_id: Uuid::new_v4().to_string(),
                ..valid.clone()
            },
            Code::NotFound,
        ),
        (
            api::SetDeviceProfileRepositoryLinkRequest {
                vendor_profile_id: 65536,
                ..valid.clone()
            },
            Code::InvalidArgument,
        ),
        (
            api::SetDeviceProfileRepositoryLinkRequest {
                device_id: String::new(),
                ..valid.clone()
            },
            Code::InvalidArgument,
        ),
        (
            api::SetDeviceProfileRepositoryLinkRequest {
                id: Uuid::new_v4().to_string(),
                ..valid.clone()
            },
            Code::NotFound,
        ),
    ];
    for (req, code) in invalid {
        assert_eq!(
            code,
            service
                .set_repository_link(request(&auth, req))
                .await
                .unwrap_err()
                .code()
        );
        assert_eq!(profile, device_profile::get(&profile.id).await.unwrap());
    }
    let tenant = tenant::create(tenant::Tenant {
        name: "Tenant".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let tenant_profile = device_profile::create(device_profile::DeviceProfile {
        name: "Tenant profile".into(),
        tenant_id: Some(tenant.id),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(
        Code::FailedPrecondition,
        service
            .set_repository_link(request(&auth, link(&tenant_profile, &model)))
            .await
            .unwrap_err()
            .code()
    );
}

#[tokio::test]
async fn catalog_update_does_not_upsert_and_validates_input() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(RequestValidator::new());
    let (model, _) = seed().await;
    let vendor_id = model.vendor_id.to_string();
    assert_eq!(
        Code::NotFound,
        service
            .update_vendor(request(
                &auth,
                api::UpdateDeviceProfileVendorRequest {
                    vendor: Some(api::DeviceProfileVendor {
                        id: Uuid::new_v4().to_string(),
                        name: "Missing".into(),
                        ..Default::default()
                    })
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(
        Code::NotFound,
        service
            .update_device(request(
                &auth,
                api::UpdateDeviceProfileDeviceRequest {
                    device: Some(api::DeviceProfileDevice {
                        id: Uuid::new_v4().to_string(),
                        vendor_id: vendor_id.clone(),
                        name: "Missing".into(),
                        ..Default::default()
                    })
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(
        Code::InvalidArgument,
        service
            .create_vendor(request(
                &auth,
                api::CreateDeviceProfileVendorRequest { vendor: None }
            ))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(
        Code::InvalidArgument,
        service
            .create_device(request(
                &auth,
                api::CreateDeviceProfileDeviceRequest { device: None }
            ))
            .await
            .unwrap_err()
            .code()
    );
    for vendor in [
        api::DeviceProfileVendor {
            name: " ".into(),
            ..Default::default()
        },
        api::DeviceProfileVendor {
            name: "Overflow".into(),
            vendor_id: u32::MAX,
            ..Default::default()
        },
    ] {
        assert_eq!(
            Code::InvalidArgument,
            service
                .create_vendor(request(
                    &auth,
                    api::CreateDeviceProfileVendorRequest {
                        vendor: Some(vendor)
                    }
                ))
                .await
                .unwrap_err()
                .code()
        );
    }
    for (vendor_id, code) in [
        ("invalid".into(), Code::InvalidArgument),
        (Uuid::new_v4().to_string(), Code::NotFound),
    ] {
        assert_eq!(
            code,
            service
                .create_device(request(
                    &auth,
                    api::CreateDeviceProfileDeviceRequest {
                        device: Some(api::DeviceProfileDevice {
                            name: "Model".into(),
                            vendor_id,
                            ..Default::default()
                        })
                    }
                ))
                .await
                .unwrap_err()
                .code()
        );
    }
    assert_eq!(
        1,
        device_profile::get_device_count(&device_profile::DeviceFilters::default())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn catalog_write_authorization() {
    let _guard = test::prepare().await;
    let service = DeviceProfile::new(RequestValidator::new());
    let (model, profile) = seed().await;
    let tenant = tenant::create(tenant::Tenant {
        name: "Tenant".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut denied = vec![AuthID::None];
    let tenant_admin = user::create(user::User {
        email: "tenant-admin@example.com".into(),
        is_active: true,
        ..Default::default()
    })
    .await
    .unwrap();
    tenant::add_user(
        tenant::TenantUser {
            tenant_id: tenant.id,
            user_id: tenant_admin.id,
            is_admin: true,
            ..Default::default()
        },
        &[],
        &[],
    )
    .await
    .unwrap();
    denied.push(AuthID::User(*tenant_admin.id));
    for (is_admin, is_active) in [(false, true), (true, false)] {
        let user = user::create(user::User {
            email: format!("{is_admin}-{is_active}@example.com"),
            is_admin,
            is_active,
            ..Default::default()
        })
        .await
        .unwrap();
        denied.push(AuthID::User(*user.id));
    }
    for (is_admin, is_read_only, tenant_id) in [(true, true, None), (false, false, Some(tenant.id))]
    {
        let key = api_key::create(api_key::ApiKey {
            name: "Denied".into(),
            is_admin,
            is_read_only,
            tenant_id,
            ..Default::default()
        })
        .await
        .unwrap();
        denied.push(AuthID::Key(*key.id));
    }
    for auth in denied {
        assert_eq!(
            Code::Unauthenticated,
            service
                .create_vendor(request(
                    &auth,
                    api::CreateDeviceProfileVendorRequest {
                        vendor: Some(api::DeviceProfileVendor {
                            name: "Forbidden".into(),
                            ..Default::default()
                        })
                    }
                ))
                .await
                .unwrap_err()
                .code()
        );
        assert_eq!(
            Code::Unauthenticated,
            service
                .update_vendor(request(
                    &auth,
                    api::UpdateDeviceProfileVendorRequest {
                        vendor: Some(api::DeviceProfileVendor {
                            id: model.vendor_id.to_string(),
                            name: "Forbidden".into(),
                            ..Default::default()
                        })
                    }
                ))
                .await
                .unwrap_err()
                .code()
        );
        let dto = api::DeviceProfileDevice {
            id: model.id.to_string(),
            vendor_id: model.vendor_id.to_string(),
            name: "Forbidden".into(),
            ..Default::default()
        };
        assert_eq!(
            Code::Unauthenticated,
            service
                .create_device(request(
                    &auth,
                    api::CreateDeviceProfileDeviceRequest {
                        device: Some(dto.clone())
                    }
                ))
                .await
                .unwrap_err()
                .code()
        );
        assert_eq!(
            Code::Unauthenticated,
            service
                .update_device(request(
                    &auth,
                    api::UpdateDeviceProfileDeviceRequest { device: Some(dto) }
                ))
                .await
                .unwrap_err()
                .code()
        );
        assert_eq!(
            Code::Unauthenticated,
            service
                .set_repository_link(request(&auth, link(&profile, &model)))
                .await
                .unwrap_err()
                .code()
        );
    }
    assert_eq!(profile, device_profile::get(&profile.id).await.unwrap());
    assert_eq!(
        "SA2-L",
        device_profile::get_device(*model.id).await.unwrap().name
    );
    assert_eq!(
        "Sontex",
        device_profile::get_vendor(*model.vendor_id)
            .await
            .unwrap()
            .name
    );
    assert_eq!(
        1,
        device_profile::get_device_count(&device_profile::DeviceFilters::default())
            .await
            .unwrap()
    );

    // A writable global API key supports the entire workflow without a user session.
    let key = api_key::create(api_key::ApiKey {
        name: "Admin".into(),
        is_admin: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let auth = AuthID::Key(*key.id);
    let mut vendor = api::DeviceProfileVendor {
        name: "API vendor".into(),
        ..Default::default()
    };
    vendor.id = service
        .create_vendor(request(
            &auth,
            api::CreateDeviceProfileVendorRequest {
                vendor: Some(vendor.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .id;
    service
        .update_vendor(request(
            &auth,
            api::UpdateDeviceProfileVendorRequest {
                vendor: Some(vendor.clone()),
            },
        ))
        .await
        .unwrap();
    let mut dto = api::DeviceProfileDevice {
        vendor_id: vendor.id,
        name: "API model".into(),
        ..Default::default()
    };
    dto.id = service
        .create_device(request(
            &auth,
            api::CreateDeviceProfileDeviceRequest {
                device: Some(dto.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .id;
    service
        .update_device(request(
            &auth,
            api::UpdateDeviceProfileDeviceRequest {
                device: Some(dto.clone()),
            },
        ))
        .await
        .unwrap();
    service
        .set_repository_link(request(
            &auth,
            api::SetDeviceProfileRepositoryLinkRequest {
                device_id: dto.id.clone(),
                ..link(&profile, &model)
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        dto.id,
        device_profile::get(&profile.id)
            .await
            .unwrap()
            .device_id
            .unwrap()
            .to_string()
    );
}

#[tokio::test]
async fn catalog_link_allows_unknown_firmware() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(RequestValidator::new());
    let (model, profile) = seed().await;
    service
        .set_repository_link(request(
            &auth,
            api::SetDeviceProfileRepositoryLinkRequest {
                firmware_version: String::new(),
                ..link(&profile, &model)
            },
        ))
        .await
        .unwrap();
    let got = device_profile::get(&profile.id).await.unwrap();
    assert_eq!(Some(model.id), got.device_id);
    assert!(got.firmware_version.is_empty());
}

#[tokio::test]
async fn catalog_link_survives_stale_profile_update() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(RequestValidator::new());
    let (model, mut stale) = seed().await;
    service
        .set_repository_link(request(&auth, link(&stale, &model)))
        .await
        .unwrap();
    stale.name = "Changed by older request".into();
    let got = device_profile::update_preserving_repository_link(stale)
        .await
        .unwrap();
    assert_eq!("Changed by older request", got.name);
    assert_eq!(Some(model.id), got.device_id);
    assert_eq!("1.2.3", got.firmware_version);
    assert_eq!(42, got.vendor_profile_id);
}
