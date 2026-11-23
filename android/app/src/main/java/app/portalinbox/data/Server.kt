package app.portalinbox.data

/**
 * One portal that this phone is enrolled to (one row of the table `servers`). In code it is still a "server".
 *
 * Two kinds of fields exist. The identity fields (`nickname`, `colorIndex`, `keyFp`) are local: the user chose them and the
 * pinned key gives the fingerprint. A screen reads them through [app.portalinbox.portal.PortalIdentity]. `reportedName` is
 * what the portal calls itself: the portal chooses it, so it is never used to name the portal on a screen.
 */
data class Server(
    val id: Long,
    val address: String,
    /** What the portal says in its `welcome` (old `serverName`). Never the identity: show it only as "The portal calls itself ...". */
    val reportedName: String,
    val deviceId: String,
    val deviceName: String,
    val deviceKeySealed: ByteArray,
    val serverPublicKey: ByteArray,
    val reenrollReason: String?,
    val heartbeatMs: Long,
    val offlineWarningMs: Long,
    /** Login of the account on that server. The server assigns it: it is read-only here. */
    val userName: String,
    /** The name that the user gave, 1 to 16 characters. Unique among the portals (after normalization). */
    val nickname: String,
    /** Index 0 to 9 of [app.portalinbox.portal.PortalPalette]. Unique among the portals. */
    val colorIndex: Int,
    /** The order of the user (the filter strip, the dots, the list). */
    val sortIndex: Int,
    /** The 20 hexadecimal characters of the fingerprint of [serverPublicKey]. Unique: the same portal cannot be added twice. */
    val keyFp: String,
    /** The phone is muted for this portal until this time (local). Null = not muted. [Long.MAX_VALUE] = until the user unmutes. */
    val mutedUntilMs: Long?,
    /** True: the lock screen text does not name the portal (local). The default is false: the name shows. */
    val hideNameOnLock: Boolean,
    /** Days to keep finished items of this portal. Null = the global setting. */
    val retentionDays: Int?,
    /** The time of the last frame of this portal (phone clock). Written at most once a minute. Null = never connected. */
    val lastSyncMs: Long?,
    /** The fingerprint (20 hexadecimal characters) of the key before the last re-enrollment with another key. */
    val prevKeyFp: String?,
    /** When that re-enrollment happened (phone clock). */
    val keyChangedMs: Long?,
    /** JSON of the account preferences that wait for the next `welcome`. */
    val pendingPrefs: String?,
    /** When the user added this portal (phone clock): the "New" mark lasts 24 hours from it. */
    val addedAtMs: Long,
)
