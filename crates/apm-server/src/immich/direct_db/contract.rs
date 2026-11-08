//! Exact migration names from v3.3.3's FileMigrationProvider (one final .ts
//! extension removed). The upstream double .ts filename intentionally keeps one.

use sqlx::{PgConnection, Row};

use super::DbMoveError;

pub(super) const MIGRATIONS: &str = "\
1744910873969-InitialMigration
1744991379464-AddNotificationsTable
1745244781846-AddUserAvatarColorColumn
1745902563899-AddAssetVisibilityColumn
1746636476623-DropExtraIndexes
1746768490606-AddUserPincode
1746844028242-AddLockedVisibilityEnum
1746987967923-AddPinExpiresAtColumn
1747329504572-AddNewSessionColumns
1747338664832-SessionRename
1747664684909-AddAlbumAuditTables
1749067526135-UserOnboardingDefault
1750107668827-PartnerCreateId
1750189909087-AddAlbumUserCreateFields
1750323941566-UnsetPrewarmDimParameter
1750676477029-AlbumAssetUpdateId
1750694237564-AlbumAssetAuditTable
1750780093818-AddAlbumToAssetDeleteTrigger
1751035357937-MemorySyncChanges
1751304834247-StackSyncChanges
1751924596408-AddOverrides
1752004072340-UpdateIndexOverrides
1752152941084-PeopleAuditTable
1752161055253-RenameGeodataPKConstraint
1752161055254-AddActivityAssetFk
1752169992364-AddIsPendingSyncReset
1752250924342-UserMetadataSync
1752267649968-StandardizeNames
1752759108283-ConvertToAbsolutePaths
1753104909784-AssetFaceUpdateIdAndAuditTable
1753464178233-RenameApiKeyPermissions
1753471866748-AddSharedLinkSlug
1753800911775-ProfileImageCheckpointRemoval
1754389095885-ResetAlbumAssetSync
1756318797207-AssetMetadataTables
1758705774125-CreateAssetOCRTable
1758705789125-CreateOCRSearchTable
1758705804128-UpsertOcrAssetJobStatus
1761078763279-AddAppVersionColumnToSession
1761755618862-FixColumnNames
1762297277677-AddPluginAndWorkflowTables
1764483051488-OCRBigramsForCJK
1764698859174-SidecarInAssetFile
1764705680041-ChangeWorkflowTableColumnsName
1764957138636-AddLockedPropertiesToAssetExif
1768336661963-AddAssetWidthHeight
1768336671610-CreateAssetEditTable
1768336694315-CreateIsVisibleColumns
1768587436457-AddEditCountToAsset
1768757482271-SwitchToIsEdited
1768828334807-AddIsEditedToAssetFile
1768847456553-AddTagsToExif
1769105700133-AddAssetEditSequence
1769441657564-AddIsProgressiveColumn
1769635093204-DropThumbnailJobStatusColumns
1771478781948-PeopleSearchIndex
1771535611395-ConvertRating0ToNull
1771639515206-AddIsTransparentColumn
1771873044511-ChangesTokensToBuffers
1771873813973-AssetEditSync
1772121424533-AddAssetExifGistEarthcoord
1772129818245-FixStupidWhiteSpace
1772609167000-UpdateOpusCodecName
1773242919341-EncodedVideoAssetFiles
1773956345315-DuplicateSharedLinkAssets
1774393726320-AssetFaceSyncReset
1774548649115-AddChecksumAlgorithm.ts
1775165531374-AddPersonNameTrigramIndex
1776217577402-DropAuditTable
1776263790468-DropDeviceIdAndDeviceAssetId
1776332807985-SetOAuthAllowInsecureRequests
1776442031775-AddOauthSidToSession
1776792304485-ReconcileSqlToolsUpgradeChanges
1776848612954-MigrateAlbumOwnerIdToAlbumUser
1777415973792-AddVideoStreamTables
1777654048096-CreateAudioVideoTables
1777667825574-ChangeDurationToInteger
1777897107000-PartnerAssetSyncReset
1778614946174-UpdateWorkflowTables
1779806699547-AddPluginTemplates
1780435471692-DeleteMismatchedAssetFaces
1780592070031-ConvertNegativeRatingToNull
1780592071031-AssetOcrSync
1781089983296-CreateIntegrityReportTable
1782414436633-AddPluginMethodAllowedHosts
1782500000000-RestoreLivePhotoStillVisibility
1784647658615-AddOAuthBearerTokenToSession
1784836013770-MinFacePreferenceMigration
1784986754473-ConvertUserPasswordEmptyStringToNull
1784986754474-AlbumDescriptionNullable
1786385711807-AlbumOwnerDeleteTrigger
1786741078327-AddWorkflowLogsTable
1786972746371-AssetOcrUpdatedAtTrigger
1786972746372-AssetOcrSyncReset
1787148183729-ClusterGroups
1787148183730-DeleteMismatchedMemoryAssets
1789419229196-ConvertUserOAuthIdEmptyStringToNull
1790587508209-RenameGeoNamesCountries
1790616293884-PersonSharing
1790693088454-AddPersonUserTableSharedBySharedWithConstraint";

pub(super) async fn verify(conn: &mut PgConnection) -> Result<(), DbMoveError> {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM public.kysely_migrations ORDER BY name")
            .fetch_all(&mut *conn)
            .await
            .map_err(|_| DbMoveError::Incompatible)?;
    if names.iter().map(String::as_str).ne(MIGRATIONS.lines()) {
        return Err(DbMoveError::Incompatible);
    }
    let engine: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
        .fetch_one(&mut *conn)
        .await
        .map_err(|_| DbMoveError::Unavailable)?;
    if engine < 140000 {
        return Err(DbMoveError::Incompatible);
    }
    let triggers_active: bool =
        sqlx::query_scalar("SELECT current_setting('session_replication_role') = 'origin'")
            .fetch_one(&mut *conn)
            .await
            .map_err(|_| DbMoveError::Incompatible)?;
    if !triggers_active {
        return Err(DbMoveError::Incompatible);
    }
    let required = [
        ("asset", "id", "uuid"),
        ("asset", "ownerId", "uuid"),
        ("asset", "libraryId", "uuid"),
        ("asset", "isExternal", "boolean"),
        ("asset", "originalPath", "character varying"),
        ("asset", "originalFileName", "character varying"),
        ("asset", "updateId", "uuid"),
        ("asset", "updatedAt", "timestamp with time zone"),
        ("asset", "deletedAt", "timestamp with time zone"),
        ("asset", "checksum", "bytea"),
        ("asset_file", "assetId", "uuid"),
        ("asset_file", "type", "character varying"),
        ("asset_file", "path", "character varying"),
        ("asset_file", "isEdited", "boolean"),
        ("asset_file", "updateId", "uuid"),
        ("library", "id", "uuid"),
        ("library", "ownerId", "uuid"),
        ("library", "importPaths", "text[]"),
        ("library", "exclusionPatterns", "text[]"),
    ];
    for (table, column, expected) in required {
        let actual: Option<String> = sqlx::query_scalar(
            "SELECT format_type(a.atttypid, a.atttypmod) FROM pg_catalog.pg_attribute a
            JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
            JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
            WHERE n.nspname = 'public' AND c.relname = $1 AND a.attname = $2
            AND NOT a.attisdropped AND c.relkind = 'r'",
        )
        .bind(table)
        .bind(column)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|_| DbMoveError::Incompatible)?;
        if actual.as_deref() != Some(expected) {
            return Err(DbMoveError::Incompatible);
        }
        let algorithms: Vec<String> = sqlx::query_scalar(
            r#"SELECT e.enumlabel FROM pg_catalog.pg_enum e
            JOIN pg_catalog.pg_attribute a ON a.atttypid = e.enumtypid
            WHERE a.attrelid = 'public.asset'::regclass AND a.attname = 'checksumAlgorithm'
            ORDER BY e.enumlabel"#,
        )
        .fetch_all(&mut *conn)
        .await
        .map_err(|_| DbMoveError::Incompatible)?;
        if algorithms != ["sha1", "sha1-path"] {
            return Err(DbMoveError::Incompatible);
        }
        let checksum_unique: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_index i
            WHERE i.indrelid = 'public.asset'::regclass AND i.indisunique AND i.indisvalid
            AND pg_get_expr(i.indpred, i.indrelid) = '("libraryId" IS NOT NULL)'
            AND ARRAY(SELECT a.attname::text FROM unnest(i.indkey) WITH ORDINALITY k(num, position)
                JOIN pg_catalog.pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.num
                ORDER BY k.position) = ARRAY['ownerId', 'libraryId', 'checksum'])"#,
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(|_| DbMoveError::Incompatible)?;
        if !checksum_unique {
            return Err(DbMoveError::Incompatible);
        }
    }
    // Require enabled ordinary BEFORE UPDATE row triggers calling the cursor function.
    // Never suppress triggers (including by changing session_replication_role).
    for (table, trigger) in [
        ("asset", "asset_updatedAt"),
        ("asset_file", "asset_file_updatedAt"),
    ] {
        let rows = sqlx::query(
            "SELECT t.tgtype::int AS kind, t.tgenabled::text AS enabled,
            p.proname AS function, p.prosrc AS body
            FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_proc p ON p.oid = t.tgfoid
            WHERE t.tgrelid = to_regclass($1) AND t.tgname = $2 AND NOT t.tgisinternal
            AND p.pronamespace = 'public'::regnamespace",
        )
        .bind(format!("public.{table}"))
        .bind(trigger)
        .fetch_all(&mut *conn)
        .await
        .map_err(|_| DbMoveError::Incompatible)?;
        let Some(row) = rows.first().filter(|_| rows.len() == 1) else {
            return Err(DbMoveError::Incompatible);
        };
        let kind: i32 = row.try_get("kind").map_err(|_| DbMoveError::Incompatible)?;
        let enabled: String = row
            .try_get("enabled")
            .map_err(|_| DbMoveError::Incompatible)?;
        let function: String = row
            .try_get("function")
            .map_err(|_| DbMoveError::Incompatible)?;
        let body: String = row.try_get("body").map_err(|_| DbMoveError::Incompatible)?;
        let body: String = body.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        if kind != 19
            || enabled != "O"
            || function != "updated_at"
            || body
                != "DECLAREclock_timestampTIMESTAMP:=clock_timestamp();BEGINnew.\"updatedAt\"=clock_timestamp;new.\"updateId\"=immich_uuid_v7(clock_timestamp);returnnew;END;"
        {
            return Err(DbMoveError::Incompatible);
        }
    }
    let unique: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_constraint
        WHERE conrelid = 'public.asset_file'::regclass AND contype = 'u'
        AND pg_get_constraintdef(oid) = 'UNIQUE ("assetId", type, "isEdited")')"#,
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(|_| DbMoveError::Incompatible)?;
    if !unique {
        return Err(DbMoveError::Incompatible);
    }
    Ok(())
}
