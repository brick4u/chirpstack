# Device-profile catalog writes

This extension builds on PR #1071, as merged into ChirpStack v4.20.2
(`fbdd5bf76b67807670a0240b11bffc7034ac09d5`). That PR allows creating global
profiles and preserves existing repository fields on ordinary updates. It does
not accept client-supplied model / firmware assignments, nor does it provide
vendor and model create / update methods.

## Operating mode

Manage the global device-profile catalog either through the catalog API or
through the repository importer. **Do not run the importer against a database
whose catalog is managed through the API.** The importer can overwrite matching
profiles, and ChirpStack does not enforce this separation.

The unchanged importer matches model UUID, region and firmware. It can replace
radio settings and codecs on an API-managed profile while retaining its UUID.
Clearing or moving an imported profile's link can instead make a subsequent
import create another profile with a new UUID. There is no automatic conflict
resolution or ownership flag. Choose one write path per database and stop
scheduled / startup imports before switching to API management.

## API

All five new methods belong to `api.DeviceProfileService`. Writes require an
active global administrator or a writable administrator API key. Tenant users,
tenant API keys and read-only keys cannot change the shared catalog.

| gRPC method | HTTP annotation | Purpose |
| --- | --- | --- |
| `CreateVendor` | `POST /api/device-profiles/vendors` | Create a manufacturer; return its UUID. |
| `UpdateVendor` | `PUT /api/device-profiles/vendors/{vendor.id}` | Update the manufacturer with this UUID. |
| `CreateDevice` | `POST /api/device-profiles/devices` | Create a catalog model under an existing manufacturer. |
| `UpdateDevice` | `PUT /api/device-profiles/devices/{device.id}` | Update an existing model, including its manufacturer. |
| `SetRepositoryLink` | `PUT /api/device-profiles/{id}/repository-link` | Assign or clear a global profile's model, firmware and optional TR005 profile identifier. |

Here, **device** means a catalog model, not a physical LoRaWAN end device. A
profile's manufacturer follows from `profile.device_id -> model.vendor_id`.
An explicit vendor assignment on the profile is therefore unnecessary.

Create methods generate UUIDs. They are not name-based upserts. Synchronizers
should use the existing `ListVendors` / `ListDevices` / `List` methods to resolve
existing records, then update by UUID. Store returned IDs; avoid concurrent
create operations for the same catalog item. Repeated assignment to the same
profile is idempotent with respect to catalog entries and profile IDs.

Update methods return `NOT_FOUND` for absent UUIDs rather than creating records.
Vendor and model names must be nonblank. A model must refer to an existing
manufacturer. `vendor.vendor_id` is the numeric LoRa Alliance vendor identifier,
not the manufacturer's UUID. It must be 0..65535, matching the two-byte VendorID
in [TR005, section 4.2.4](https://lora-alliance.org/wp-content/uploads/2020/11/TR005_LoRaWAN_Device_Identification_QR_Codes.pdf#page=8).
Model names are limited to 100 Unicode characters and firmware versions to 20,
on both PostgreSQL and SQLite.

The existing `DeleteVendor` and `DeleteDevice` methods return
`FAILED_PRECONDITION` while any profile is linked to the vendor or model. Unlink
those profiles explicitly before deleting catalog entries. This protects both
the profiles and provisioned LoRaWAN devices from the existing foreign-key
cascade. Deleting an unlinked vendor can still delete its unlinked models.

### TR005 identifiers

The pair `(numeric vendor_id, nonzero vendor_profile_id)` can resolve to at most
one profile, across models and vendor UUIDs. Zero is unassigned and
`GetByProfileId` returns `NOT_FOUND` for it. Existing ambiguous data returns
`FAILED_PRECONDITION` instead of selecting an arbitrary profile.

Assignment changes, model moves between vendors, and vendor numeric-ID changes
validate uniqueness within a write transaction. Conflicts return `ALREADY_EXISTS`
and roll back the complete change, including its timestamp. These PostgreSQL
mutations share a transaction-scoped advisory lock across server instances;
SQLite uses `BEGIN IMMEDIATE`. Catalog deletion uses the same serialization so
it cannot race a new assignment. These protections do not apply to the unchanged
importer or direct SQL writes; observe the operating-mode restriction above.

Multiple profiles for the same model, region and firmware are allowed. Give
variants distinct TR005 identifiers or leave their identifier at zero.

## Assign an existing global profile

Use an administrator API token, an existing model UUID and the UUID of the
existing global profile. For example, with a patched server and gRPC reflection:

```sh
grpcurl -H "authorization: Bearer ${CHIRPSTACK_API_TOKEN}" \
  -d '{
    "id": "<existing-global-profile-uuid>",
    "deviceId": "<existing-catalog-model-uuid>",
    "firmwareVersion": "1.2.3",
    "vendorProfileId": 0
  }' \
  "${CHIRPSTACK_GRPC_HOST}:443" api.DeviceProfileService/SetRepositoryLink
```

`deviceId` must identify an existing model. `firmwareVersion` may be empty if
unknown; a firmware version without a model is rejected. The optional
TR005 `vendorProfileId` is 0..65535; use zero when no such identifier is assigned.
This is not a physical device ID or an application ID.

To clear an assignment, send the same profile ID with `deviceId: ""`,
`firmwareVersion: ""` and `vendorProfileId: 0`. This does not delete the profile,
model or manufacturer. Assignments on tenant-owned profiles are rejected with
`FAILED_PRECONDITION`.

`Get` and `GetByProfileId` return `deviceId`, `firmwareVersion` and the read-only
`vendorProfileId`. When changing only the firmware, first read the profile and
send back its existing `vendorProfileId` to preserve it; omitting it means zero.
`List` with `globalOnly: true`
and `deviceId` filters the assigned global profiles. Existing lookup methods
for model / region / firmware and TR005 use the stored assignment directly.

Ordinary `Create` / `Update` requests retain their existing repository-field
semantics: fields on the profile DTO are read-only. A separate explicit method
avoids clearing assignments when older clients omit these proto3 fields.
Normal updates exclude all three repository columns from their SQL update,
including when an update was read before a concurrent assignment change.

The assignment writes only its three columns and `updated_at`. Profile UUID,
codec, radio parameters, tags and other configuration remain intact. No database
migration or importer job is needed for these new API operations.

## Build and validation

The canonical protobuf, its checked-in Rust build copy, and regenerated Go
bindings are included. Rust server / client bindings are generated by
`api/rust/build.rs`. Rebuild ChirpStack to expose the new gRPC methods. Regenerate
other SDKs using the repository's API build targets before calling these methods
through generated clients.

**REST:** HTTP annotations alone do not update a running REST adapter. Regenerate
its grpc-gateway bindings from the patched protobuf and rebuild / deploy that
adapter as well before using the HTTP paths above. Existing REST binaries do
not expose the new routes or return the newly added `vendorProfileId` field.

Backend regression tests (only against disposable local test databases;
PostgreSQL must have `pg_trgm` and `hstore` enabled, as in the repository's
`.docker-compose/postgresql/initdb` scripts):

```sh
export TEST_POSTGRESQL_DSN='postgres://postgres:<test-password>@127.0.0.1:25432/postgres'
export TEST_REDIS_URL='redis://127.0.0.1:26379'
cargo fmt --all --check
RUST_MIN_STACK=8388608 TZ=UTC cargo test --locked -p chirpstack \
  --no-default-features --features postgres api::device_profile:: \
  -- --test-threads=1
# A SQLite file prefix creates a fresh shared database for each individual test.
TEST_SQLITE_PATH=/tmp/chirpstack-catalog-tests.sqlite \
  RUST_MIN_STACK=8388608 TZ=UTC cargo test --locked -p chirpstack \
  --no-default-features --features sqlite api::device_profile:: \
  -- --test-threads=1
```

The test harness resets the configured database and Redis. The catalog tests
cover vendor / model writes and reads, changing a model's manufacturer,
assignment / clearing / repeated assignment, validation and missing records,
tenant-profile rejection, administrator API keys, rejected identities, and
preservation of assignments during stale ordinary profile updates. Review
regressions additionally cover linked deletion with provisioned devices,
ambiguous legacy IDs, zero / duplicate TR005 identifiers, model and vendor moves,
read-modify-write preservation, Unicode length / numeric-ID boundaries, and
concurrent link / model-move / vendor-ID writes. Existing
profile tests additionally cover global creation and normal tenant updates.
