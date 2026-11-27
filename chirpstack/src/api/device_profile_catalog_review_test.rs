use super::catalog_test::{admin, link, request, seed};
use super::*;
use crate::storage::{application, device, tenant};
use crate::test;
use tonic::Code;

async fn fixture(vendor_id: i32) -> (device_profile::Device, device_profile::DeviceProfile) {
    let (model, profile) = seed().await;
    device_profile::update_vendor(device_profile::Vendor {
        id: model.vendor_id,
        name: format!("Vendor {vendor_id}"),
        vendor_id,
        ..Default::default()
    })
    .await
    .unwrap();
    (model, profile)
}

#[tokio::test]
async fn linked_catalog_deletion_preserves_profiles_and_provisioned_devices() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(validator::RequestValidator::new());
    let (model, profile) = seed().await;
    let tenant = tenant::create(tenant::Tenant {
        name: "Provisioned tenant".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let app = application::create(application::Application {
        name: "Application".into(),
        tenant_id: tenant.id,
        ..Default::default()
    })
    .await
    .unwrap();
    let device = device::create(device::Device {
        name: "Provisioned device".into(),
        application_id: app.id,
        device_profile_id: profile.id,
        dev_eui: lrwn::EUI64::from_be_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
        ..Default::default()
    })
    .await
    .unwrap();
    service
        .set_repository_link(request(&auth, link(&profile, &model)))
        .await
        .unwrap();
    let linked = device_profile::get(&profile.id).await.unwrap();
    assert_eq!(
        Code::FailedPrecondition,
        service
            .delete_device(request(
                &auth,
                api::DeleteDeviceProfileDeviceRequest {
                    id: model.id.to_string()
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(
        Code::FailedPrecondition,
        service
            .delete_vendor(request(
                &auth,
                api::DeleteDeviceProfileVendorRequest {
                    id: model.vendor_id.to_string()
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(linked, device_profile::get(&profile.id).await.unwrap());
    assert_eq!(device, device::get(&device.dev_eui).await.unwrap());
    assert_eq!(model, device_profile::get_device(*model.id).await.unwrap());

    // Removing the link makes catalog deletion safe and retains deployed devices.
    service
        .set_repository_link(request(
            &auth,
            api::SetDeviceProfileRepositoryLinkRequest {
                id: profile.id.to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    service
        .delete_vendor(request(
            &auth,
            api::DeleteDeviceProfileVendorRequest {
                id: model.vendor_id.to_string(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        None,
        device_profile::get(&profile.id).await.unwrap().device_id
    );
    assert_eq!(device, device::get(&device.dev_eui).await.unwrap());
    assert!(device_profile::get_device(*model.id).await.is_err());
}

#[tokio::test]
async fn tr005_link_conflicts_roll_back_and_zero_allows_variants() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(validator::RequestValidator::new());
    let (model, profile) = seed().await;
    let other = device_profile::create(device_profile::DeviceProfile {
        name: "Same hardware variant".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    service
        .set_repository_link(request(&auth, link(&profile, &model)))
        .await
        .unwrap();
    assert_eq!(
        Code::AlreadyExists,
        service
            .set_repository_link(request(&auth, link(&other, &model)))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(other, device_profile::get(&other.id).await.unwrap());
    // A different vendor UUID with the same numeric VendorID cannot reuse the pair.
    let (other_model, other_profile) = seed().await;
    assert_eq!(
        Code::AlreadyExists,
        service
            .set_repository_link(request(&auth, link(&other_profile, &other_model)))
            .await
            .unwrap_err()
            .code()
    );
    for p in [&profile, &other, &other_profile] {
        service
            .set_repository_link(request(
                &auth,
                api::SetDeviceProfileRepositoryLinkRequest {
                    vendor_profile_id: 0,
                    ..link(p, &model)
                },
            ))
            .await
            .unwrap();
    }
    // Identical model / region / firmware remain valid profile variants.
    let profiles = service
        .list(request(
            &auth,
            api::ListDeviceProfilesRequest {
                global_only: true,
                device_id: model.id.to_string(),
                limit: 10,
                ..Default::default()
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(3, profiles.total_count);
    assert_eq!(
        Code::NotFound,
        service
            .get_by_profile_id(request(
                &auth,
                api::GetDeviceProfileByProfileIdRequest {
                    vendor_id: 123,
                    vendor_profile_id: 0
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
}

#[tokio::test]
async fn tr005_model_moves_and_vendor_id_changes_are_atomic() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(validator::RequestValidator::new());
    let (target_model, target_profile) = fixture(123).await;
    let (source_model, source_profile) = fixture(456).await;
    service
        .set_repository_link(request(&auth, link(&target_profile, &target_model)))
        .await
        .unwrap();
    service
        .set_repository_link(request(&auth, link(&source_profile, &source_model)))
        .await
        .unwrap();
    let source_vendor = device_profile::get_vendor(*source_model.vendor_id)
        .await
        .unwrap();
    let mut model_dto = service
        .get_device(request(
            &auth,
            api::GetDeviceProfileDeviceRequest {
                id: source_model.id.to_string(),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .device
        .unwrap();
    model_dto.vendor_id = target_model.vendor_id.to_string();
    assert_eq!(
        Code::AlreadyExists,
        service
            .update_device(request(
                &auth,
                api::UpdateDeviceProfileDeviceRequest {
                    device: Some(model_dto.clone())
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(
        source_model,
        device_profile::get_device(*source_model.id).await.unwrap()
    );
    let mut vendor_dto = service
        .get_vendor(request(
            &auth,
            api::GetDeviceProfileVendorRequest {
                id: source_vendor.id.to_string(),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .vendor
        .unwrap();
    vendor_dto.vendor_id = 123;
    assert_eq!(
        Code::AlreadyExists,
        service
            .update_vendor(request(
                &auth,
                api::UpdateDeviceProfileVendorRequest {
                    vendor: Some(vendor_dto.clone())
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(
        source_vendor,
        device_profile::get_vendor(*source_vendor.id).await.unwrap()
    );
    assert_eq!(
        source_profile.id,
        device_profile::get_by_profile_id(456, 42).await.unwrap().id
    );

    // Same numeric vendor ID is allowed when the profile identifiers differ.
    service
        .set_repository_link(request(
            &auth,
            api::SetDeviceProfileRepositoryLinkRequest {
                vendor_profile_id: 43,
                ..link(&source_profile, &source_model)
            },
        ))
        .await
        .unwrap();
    service
        .update_vendor(request(
            &auth,
            api::UpdateDeviceProfileVendorRequest {
                vendor: Some(vendor_dto),
            },
        ))
        .await
        .unwrap();
    service
        .update_device(request(
            &auth,
            api::UpdateDeviceProfileDeviceRequest {
                device: Some(model_dto),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        source_profile.id,
        device_profile::get_by_profile_id(123, 43).await.unwrap().id
    );
    assert_eq!(
        target_profile.id,
        device_profile::get_by_profile_id(123, 42).await.unwrap().id
    );
}

#[tokio::test]
async fn tr005_existing_ambiguity_is_reported_and_can_be_repaired() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(validator::RequestValidator::new());
    let (model, profile) = seed().await;
    // Simulate ambiguous pre-existing data from before the catalog API.
    let first = device_profile::create(device_profile::DeviceProfile {
        name: "Legacy one".into(),
        device_id: Some(model.id),
        vendor_profile_id: 42,
        ..Default::default()
    })
    .await
    .unwrap();
    let second = device_profile::create(device_profile::DeviceProfile {
        name: "Legacy two".into(),
        device_id: Some(model.id),
        vendor_profile_id: 42,
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(
        Code::FailedPrecondition,
        service
            .get_by_profile_id(request(
                &auth,
                api::GetDeviceProfileByProfileIdRequest {
                    vendor_id: 123,
                    vendor_profile_id: 42
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
    service
        .set_repository_link(request(
            &auth,
            api::SetDeviceProfileRepositoryLinkRequest {
                id: second.id.to_string(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        first.id,
        device_profile::get_by_profile_id(123, 42).await.unwrap().id
    );
    assert_eq!(profile, device_profile::get(&profile.id).await.unwrap());
}

#[tokio::test]
async fn tr005_id_survives_read_modify_write() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(validator::RequestValidator::new());
    let (model, profile) = seed().await;
    service
        .set_repository_link(request(&auth, link(&profile, &model)))
        .await
        .unwrap();
    let got = service
        .get(request(
            &auth,
            api::GetDeviceProfileRequest {
                id: profile.id.to_string(),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .device_profile
        .unwrap();
    assert_eq!(42, got.vendor_profile_id);
    service
        .set_repository_link(request(
            &auth,
            api::SetDeviceProfileRepositoryLinkRequest {
                id: got.id,
                device_id: got.device_id,
                firmware_version: "2.0".into(),
                vendor_profile_id: got.vendor_profile_id,
            },
        ))
        .await
        .unwrap();
    let got = service
        .get_by_profile_id(request(
            &auth,
            api::GetDeviceProfileByProfileIdRequest {
                vendor_id: 123,
                vendor_profile_id: 42,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .device_profile
        .unwrap();
    assert_eq!(42, got.vendor_profile_id);
    assert_eq!("2.0", got.firmware_version);
}

#[tokio::test]
async fn catalog_validation_boundaries_and_unicode() {
    let _guard = test::prepare().await;
    let auth = admin().await;
    let service = DeviceProfile::new(validator::RequestValidator::new());
    let (model, profile) = seed().await;
    for vendor_id in [65536, u32::MAX] {
        assert_eq!(
            Code::InvalidArgument,
            service
                .get_by_profile_id(request(
                    &auth,
                    api::GetDeviceProfileByProfileIdRequest {
                        vendor_id,
                        vendor_profile_id: 42
                    }
                ))
                .await
                .unwrap_err()
                .code()
        );
        assert_eq!(
            Code::InvalidArgument,
            service
                .get_by_profile_id(request(
                    &auth,
                    api::GetDeviceProfileByProfileIdRequest {
                        vendor_id: 123,
                        vendor_profile_id: vendor_id
                    }
                ))
                .await
                .unwrap_err()
                .code()
        );
        let vendor = api::DeviceProfileVendor {
            id: model.vendor_id.to_string(),
            name: "Out of range".into(),
            vendor_id,
            ..Default::default()
        };
        assert_eq!(
            Code::InvalidArgument,
            service
                .create_vendor(request(
                    &auth,
                    api::CreateDeviceProfileVendorRequest {
                        vendor: Some(vendor.clone())
                    }
                ))
                .await
                .unwrap_err()
                .code()
        );
        assert_eq!(
            Code::InvalidArgument,
            service
                .update_vendor(request(
                    &auth,
                    api::UpdateDeviceProfileVendorRequest {
                        vendor: Some(vendor)
                    }
                ))
                .await
                .unwrap_err()
                .code()
        );
    }
    let max_vendor = service
        .create_vendor(request(
            &auth,
            api::CreateDeviceProfileVendorRequest {
                vendor: Some(api::DeviceProfileVendor {
                    name: "Maximum".into(),
                    vendor_id: 65535,
                    ..Default::default()
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .id;
    assert_eq!(
        65535,
        device_profile::get_vendor(Uuid::from_str(&max_vendor).unwrap())
            .await
            .unwrap()
            .vendor_id
    );
    let model_dto = api::DeviceProfileDevice {
        id: model.id.to_string(),
        name: "界".repeat(101),
        vendor_id: model.vendor_id.to_string(),
        ..Default::default()
    };
    assert_eq!(
        Code::InvalidArgument,
        service
            .create_device(request(
                &auth,
                api::CreateDeviceProfileDeviceRequest {
                    device: Some(model_dto.clone())
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(
        Code::InvalidArgument,
        service
            .update_device(request(
                &auth,
                api::UpdateDeviceProfileDeviceRequest {
                    device: Some(model_dto.clone())
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(model, device_profile::get_device(*model.id).await.unwrap());
    let max_name = api::DeviceProfileDevice {
        name: "界".repeat(100),
        ..model_dto
    };
    service
        .create_device(request(
            &auth,
            api::CreateDeviceProfileDeviceRequest {
                device: Some(max_name.clone()),
            },
        ))
        .await
        .unwrap();
    service
        .update_device(request(
            &auth,
            api::UpdateDeviceProfileDeviceRequest {
                device: Some(max_name),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        Code::InvalidArgument,
        service
            .set_repository_link(request(
                &auth,
                api::SetDeviceProfileRepositoryLinkRequest {
                    firmware_version: "ä".repeat(21),
                    ..link(&profile, &model)
                }
            ))
            .await
            .unwrap_err()
            .code()
    );
    assert_eq!(profile, device_profile::get(&profile.id).await.unwrap());
    service
        .set_repository_link(request(
            &auth,
            api::SetDeviceProfileRepositoryLinkRequest {
                firmware_version: "ä".repeat(20),
                ..link(&profile, &model)
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        "ä".repeat(20),
        device_profile::get(&profile.id)
            .await
            .unwrap()
            .firmware_version
    );
}

#[tokio::test]
async fn concurrent_tr005_writes_have_one_winner() {
    let _guard = test::prepare().await;
    // Three competing operation pairs: link/link, link/model move, link/vendor ID.
    // TEST_SQLITE_PATH uses a disposable file so SQLite connections share a DB.
    for operation in 0..3 {
        let target_id = 1000 + operation * 10;
        let (target_model, target_profile) = fixture(target_id).await;
        let (source_model, source_profile) = fixture(target_id + 1).await;
        if operation != 0 {
            device_profile::set_repository_link(
                *source_profile.id,
                Some(*source_model.id),
                "1.0",
                42,
            )
            .await
            .unwrap();
        }
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let first_barrier = barrier.clone();
        let target_model_id = *target_model.id;
        let target_profile_id = *target_profile.id;
        let first = tokio::spawn(async move {
            first_barrier.wait().await;
            device_profile::set_repository_link(target_profile_id, Some(target_model_id), "1.0", 42)
                .await
                .map(|_| ())
        });
        let second_barrier = barrier.clone();
        let second = tokio::spawn(async move {
            second_barrier.wait().await;
            match operation {
                0 => device_profile::set_repository_link(
                    *source_profile.id,
                    Some(target_model_id),
                    "1.0",
                    42,
                )
                .await
                .map(|_| ()),
                1 => device_profile::update_device(device_profile::Device {
                    id: source_model.id,
                    vendor_id: target_model.vendor_id,
                    name: "Moved model".into(),
                    ..Default::default()
                })
                .await
                .map(|_| ()),
                _ => device_profile::update_vendor(device_profile::Vendor {
                    id: source_model.vendor_id,
                    vendor_id: target_id,
                    name: "Changed vendor".into(),
                    ..Default::default()
                })
                .await
                .map(|_| ()),
            }
        });
        barrier.wait().await;
        let (a, b) = tokio::join!(first, second);
        let a = a.unwrap();
        let b = b.unwrap();
        assert_ne!(
            a.is_ok(),
            b.is_ok(),
            "exactly one competing write must commit"
        );
        assert_eq!(
            Code::AlreadyExists,
            a.err().or(b.err()).unwrap().status().code()
        );
        device_profile::get_by_profile_id(target_id, 42)
            .await
            .unwrap();
    }
}
