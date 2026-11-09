package app.portalinbox.portal

import app.portalinbox.net.ServerAddress

/**
 * What the confirmation screen of an enrollment shows and what it lets the user do (proposal 3.5 and 3.6). Pure Kotlin. The
 * screen holds the code, the way it came, the portals of the phone and what the user typed so far. It calls [plan] after each
 * change and draws the [Plan]. The screen decides nothing.
 *
 * The code is a one-time secret. [plan] only reads: nothing here uses the code. The screen uses it when the user taps "Add portal"
 * and [Plan.canAdd] is true.
 *
 * Rules, in the order that [plan] applies them:
 * 1. A link can never re-enroll a portal in place ([Blocker.LinkCannotReenroll]). Only the screen of a portal starts [Mode.Reenroll].
 * 2. A code whose key is already held by a portal is never added a second time ([Blocker.AlreadyAdded]). The screen offers to open it.
 * 3. The phone holds at most [PortalAdmission.MAX_PORTALS] portals ([Blocker.CapReached]).
 * 4. The nickname is required and the phone proposes none. The color has a default: the first free one.
 * 5. The tick "I know who runs this portal" is required for a link, for a look-alike finding and for a re-enrollment.
 */
object EnrollPlan {
    /** How the code reached the screen. */
    sealed interface Source {
        /** The user typed or pasted the code. */
        object Typed : Source

        /** Another app opened the link. [referrer] is what Android says about that app, as a URI, or null. It is a hint, never proof. */
        data class Link(val referrer: String?) : Source
    }

    sealed interface Mode {
        /** A new portal. */
        object Add : Mode

        /** The same portal again, with a new code: it keeps its row, its nickname, its color and its history. */
        data class Reenroll(val portal: KnownPortal) : Mode
    }

    /** What the code holds: where to connect and the key to pin. [address] is the text of `ServerAddress.http`. [keyFp] is in any form. */
    data class Code(val address: String, val keyFp: String)

    /** What the user typed and chose. The nickname starts empty. A null color means "the first free color". */
    data class Entry(val nickname: String = "", val colorIndex: Int? = null, val ticked: Boolean = false)

    /** Why the tick is needed. The screen writes one line for it. */
    enum class TickReason { REENROLL, LOOK_ALIKE, LINK }

    sealed interface Blocker {
        /** The key is the key of [portal]. Nothing is added. */
        data class AlreadyAdded(val portal: KnownPortal) : Blocker

        /** Re-enrollment only: the new key is the key of another portal. */
        data class KeyOfOtherPortal(val portal: KnownPortal) : Blocker

        object CapReached : Blocker

        object LinkCannotReenroll : Blocker

        /** The address or the fingerprint of the code cannot be read. */
        object BadCode : Blocker
    }

    /** One swatch of the picker. [usedBy] is the portal that has this color: the swatch is crossed out and cannot be chosen. */
    data class ColorChoice(val index: Int, val usedBy: PortalIdentity?) {
        val free: Boolean get() = usedBy == null
    }

    data class Plan(
        val mode: Mode,
        val source: Source,
        val blocker: Blocker?,
        /** The finding against the other portals of the phone. In [Mode.Reenroll] the portal itself is not compared. */
        val finding: LookAlike.Finding,
        /** True for a finding that is a danger and not a plain fact: a look-alike address, the same address with another key, a look-alike name. */
        val lookAlike: Boolean,
        /** True when the code came from a link. The screen shows the warning. */
        val fromLink: Boolean,
        /** The referrer to show after "Opened from:", or null for "unknown". Meaningful when [fromLink]. */
        val referrerText: String?,
        /** The address as a person reads it, without the scheme. Punycode is decoded. */
        val shownAddress: String,
        /** The address of the look-alike portal, in the same form, when [finding] is an address finding. Null otherwise. */
        val otherAddress: String?,
        /** The ranges of [shownAddress] that differ from [otherAddress]. Empty when there is nothing to mark. */
        val changedRanges: List<IntRange>,
        /** True when the host holds a character outside ASCII or is written in Punycode: the screen marks it. */
        val international: Boolean,
        /** The fingerprint of the key of the code, in groups. Empty when the code is unreadable. */
        val fingerprint: String,
        /** Re-enrollment: the fingerprint of the key that the portal holds now. */
        val previousFingerprint: String?,
        /** Re-enrollment: true when the new key is not the old one. */
        val keyChanged: Boolean,
        val tickRequired: Boolean,
        val tickReason: TickReason?,
        /** Re-enrollment: the nickname is fixed and the field is not shown. */
        val fixedNickname: String?,
        /** The check of the nickname that the user typed. Null when the nickname is fixed. */
        val nickname: PortalNaming.Check?,
        /** The 10 swatches of the picker. */
        val colors: List<ColorChoice>,
        /** The color that applies: the chosen one, or the first free one. Null when none is free or the choice is not possible. */
        val colorIndex: Int?,
        /** True when the button "Add portal" (or "Enroll again") can be used. */
        val canAdd: Boolean,
    )

    fun plan(code: Code, source: Source, mode: Mode, existing: List<KnownPortal>, entry: Entry): Plan {
        val reenroll = mode as? Mode.Reenroll
        val others = if (reenroll == null) existing else existing.filter { it.identity.id != reenroll.portal.identity.id }
        val fp = Fingerprint.parse(code.keyFp)
        val parsed = ServerAddress.parse(code.address)
        val fromLink = source is Source.Link

        // The nickname check. In re-enroll mode the nickname is the one of the portal and nothing is typed.
        val nickname = if (reenroll == null) PortalNaming.validate(entry.nickname, existing.map { it.identity }) else null
        val finding = if (fp == null) {
            LookAlike.Finding.None
        } else {
            LookAlike.check(code.address, fp, others, newNickname = if (reenroll == null) entry.nickname else null)
        }
        val lookAlike = finding is LookAlike.Finding.SameAddressOtherKey ||
            finding is LookAlike.Finding.SimilarAddress || finding is LookAlike.Finding.SimilarName

        val blocker: Blocker? = when {
            fromLink && reenroll != null -> Blocker.LinkCannotReenroll
            fp == null || parsed == null -> Blocker.BadCode
            finding is LookAlike.Finding.SameKey ->
                if (reenroll == null) Blocker.AlreadyAdded(finding.portal) else Blocker.KeyOfOtherPortal(finding.portal)
            reenroll == null && existing.size >= PortalAdmission.MAX_PORTALS -> Blocker.CapReached
            else -> null
        }

        val tickReason = when {
            reenroll != null -> TickReason.REENROLL
            lookAlike -> TickReason.LOOK_ALIKE
            fromLink -> TickReason.LINK
            else -> null
        }

        val shown = parsed?.let { shownAddress(it.http) }.orEmpty()
        val other = when (finding) {
            is LookAlike.Finding.SameAddressOtherKey -> shownAddress(finding.portal.address)
            is LookAlike.Finding.SimilarAddress -> shownAddress(finding.portal.address)
            else -> null
        }
        val ranges = if (other != null && finding is LookAlike.Finding.SimilarAddress) AddressDiff.changed(shown, other) else emptyList()

        val colors = (0 until PortalPalette.SIZE).map { i -> ColorChoice(i, others.firstOrNull { it.identity.colorIndex == i }?.identity) }
        val colorIndex: Int? = when {
            reenroll != null -> reenroll.portal.identity.colorIndex
            entry.colorIndex != null -> entry.colorIndex.takeIf { PortalPalette.isValid(it) && colors[it].free }
            else -> PortalPalette.nextFree(existing.map { it.identity.colorIndex }, existing.lastOrNull()?.identity?.colorIndex)
        }

        val previous = reenroll?.portal?.identity?.fingerprint
        val nicknameOk = reenroll != null || nickname is PortalNaming.Check.Valid
        val tickOk = tickReason == null || entry.ticked
        val canAdd = blocker == null && nicknameOk && colorIndex != null && tickOk

        return Plan(
            mode = mode, source = source, blocker = blocker, finding = finding, lookAlike = lookAlike,
            fromLink = fromLink, referrerText = (source as? Source.Link)?.let { ReferrerHint.of(it.referrer) },
            shownAddress = shown, otherAddress = other, changedRanges = ranges,
            international = shown.any { it.code >= 0x80 } || code.address.contains("xn--", ignoreCase = true),
            fingerprint = fp?.let { Fingerprint.display(it) }.orEmpty(),
            previousFingerprint = previous,
            keyChanged = fp != null && previous != null && !Fingerprint.same(previous, fp),
            tickRequired = tickReason != null, tickReason = tickReason,
            fixedNickname = reenroll?.portal?.identity?.nickname, nickname = nickname,
            colors = colors, colorIndex = colorIndex, canAdd = canAdd,
        )
    }

    /** The address without the scheme, with the host as a person reads it (Punycode decoded). */
    fun shownAddress(http: String): String {
        val parts = LookAlike.parts(http) ?: return http.removePrefix("https://").removePrefix("http://")
        val rest = http.removePrefix("https://").removePrefix("http://")
        val hostEnd = rest.indexOfFirst { it == ':' || it == '/' }.let { if (it < 0) rest.length else it }
        return parts.host + rest.substring(hostEnd)
    }
}
