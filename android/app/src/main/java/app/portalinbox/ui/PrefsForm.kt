package app.portalinbox.ui

import app.portalinbox.proto.DigestMode
import app.portalinbox.proto.DigestPrefs
import app.portalinbox.proto.Prefs
import app.portalinbox.proto.QuietPrefs

/**
 * The form of the preferences screen, as plain values. Pure Kotlin: the Activity reads and writes a [Draft]. [toPrefs] makes
 * the value that goes to the server and says why a draft is not valid. Nothing is changed to make it fit: a window with
 * the same start and end is refused, not shifted.
 */
object PrefsForm {
    const val DEFAULT_QUIET_FROM = "22:00"
    const val DEFAULT_QUIET_TO = "07:00"
    const val DEFAULT_DIGEST_AT = "09:00"
    const val DEFAULT_EVERY_H = 6
    val EVERY_H_RANGE = 1..168

    data class Draft(
        val quietOn: Boolean,
        val quietFrom: String,
        val quietTo: String,
        val quietTz: String,
        val highRings: Boolean,
        val digestMode: DigestMode,
        val digestAt: String,
        val digestTz: String,
        val everyH: Int,
        /** True when the account has no digest preference: the default of the server file applies. */
        val digestUnset: Boolean,
    )

    /** The form for the values that apply now. [phoneTz] is the zone of this phone: the zone of a new window. */
    fun draftOf(prefs: Prefs, phoneTz: String): Draft {
        val q = prefs.quiet
        val d = prefs.digest
        return Draft(
            quietOn = q != null,
            quietFrom = q?.from ?: DEFAULT_QUIET_FROM,
            quietTo = q?.to ?: DEFAULT_QUIET_TO,
            quietTz = q?.tz ?: phoneTz,
            highRings = q?.highRings ?: false,
            digestMode = d?.mode ?: DigestMode.DAILY,
            digestAt = d?.at ?: DEFAULT_DIGEST_AT,
            digestTz = d?.tz ?: phoneTz,
            everyH = d?.everyH ?: DEFAULT_EVERY_H,
            digestUnset = d == null,
        )
    }

    sealed interface Result {
        data class Ok(val prefs: Prefs) : Result

        data class Refused(val reason: String) : Result
    }

    /**
     * The value for the server. A digest that the owner never touched stays unset (the default of the server file keeps
     * applying): the form writes a digest only when [Draft.digestUnset] is false.
     */
    fun toPrefs(d: Draft): Result {
        if (d.quietOn && d.quietFrom == d.quietTo) return Result.Refused("The quiet hours must start and end at different times.")
        if (d.digestMode == DigestMode.EVERY && d.everyH !in EVERY_H_RANGE) {
            return Result.Refused("The summary repeats every ${EVERY_H_RANGE.first} to ${EVERY_H_RANGE.last} hours.")
        }
        val quiet = if (d.quietOn) QuietPrefs(d.quietFrom, d.quietTo, d.quietTz, d.highRings) else null
        val digest = if (d.digestUnset) null else when (d.digestMode) {
            DigestMode.OFF -> DigestPrefs(DigestMode.OFF, null, null, null)
            DigestMode.DAILY -> DigestPrefs(DigestMode.DAILY, d.digestAt, d.digestTz, null)
            DigestMode.EVERY -> DigestPrefs(DigestMode.EVERY, null, null, d.everyH)
        }
        return Result.Ok(Prefs(quiet, digest))
    }

    /** What the screen says about the values: the portal has them, or the phone keeps an edit, or the portal refused it. */
    enum class Status { CONFIRMED, WAITING, WAITING_OFFLINE, REFUSED }

    /**
     * [pending]: the phone holds an edit that the portal did not show yet. [connected]: the portal is connected now.
     * [refused]: the last `set_prefs` got `invalid_prefs`. An edit made after the refusal is pending again, so the screen
     * says "waiting" and not "refused".
     */
    fun status(pending: Boolean, connected: Boolean, refused: Boolean): Status = when {
        pending && connected -> Status.WAITING
        pending -> Status.WAITING_OFFLINE
        refused -> Status.REFUSED
        else -> Status.CONFIRMED
    }

    private const val REFUSAL_MAX = 160

    /**
     * The line that shows a refusal of the portal. The text of the portal is data: hidden characters are shown as marks, white space
     * is one space, and the length is cut. The refused values are not on the phone any more.
     */
    fun refusalLine(message: String): String {
        val one = VisibleText.escapeLine(message.replace(Regex("\\s+"), " ").trim())
        return if (one.length > REFUSAL_MAX) one.take(REFUSAL_MAX - 1) + "…" else one
    }

    /** "Quiet from 22:00 to 07:00" for the row of the settings screen. */
    fun quietTitle(q: QuietPrefs): String = "Quiet from ${q.from} to ${q.to}"

    /** "Europe/Paris. No sound. Items still arrive." */
    fun quietDetail(q: QuietPrefs): String = "${q.tz}. No sound. Items still arrive."

    /** "Daily at 09:00", "Every 6 hours", "No summary" and "Portal default" for the row of the settings screen. */
    fun digestTitle(d: DigestPrefs?): String = when (d?.mode) {
        null -> "Portal default"
        DigestMode.OFF -> "No summary"
        DigestMode.DAILY -> "Daily at ${d.at}"
        DigestMode.EVERY -> if (d.everyH == 1) "Every hour" else "Every ${d.everyH} hours"
    }

    /** The time zone of a daily summary. Null for the other modes. */
    fun digestDetail(d: DigestPrefs?): String? = if (d?.mode == DigestMode.DAILY) d.tz else null

    /** "Quiet 22:00 to 07:00 · Daily summary at 09:00" for the row of the server screen. */
    fun summary(prefs: Prefs): String {
        val quiet = prefs.quiet?.let { "Quiet ${it.from} to ${it.to}" } ?: "No quiet hours"
        val digest = when (prefs.digest?.mode) {
            null -> "Summary: server default"
            DigestMode.OFF -> "No summary"
            DigestMode.DAILY -> "Daily summary at ${prefs.digest.at}"
            DigestMode.EVERY -> "Summary every ${prefs.digest.everyH} h"
        }
        return "$quiet · $digest"
    }

    /** The hour and the minute of "HH:MM", for the time picker. */
    fun hour(hhmm: String): Int = hhmm.substring(0, 2).toInt()

    fun minute(hhmm: String): Int = hhmm.substring(3, 5).toInt()

    fun hhmm(hour: Int, minute: Int): String = "%02d:%02d".format(hour, minute)
}
