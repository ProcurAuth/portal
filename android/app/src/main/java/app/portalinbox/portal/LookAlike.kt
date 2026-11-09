package app.portalinbox.portal

import app.portalinbox.net.ServerAddress
import java.net.IDN
import java.net.URI
import java.util.Locale

/**
 * The look-alike checks of an enrollment (proposal 2, attackers B and C; proposal 3.5). Pure Kotlin. They compare the
 * portal that the user is about to add with the portals that the phone holds. The result is a [Finding]. The screen of the
 * enrollment (lot MP2) turns it into a banner and into the required tick.
 *
 * The checks, from the strongest finding to the weakest:
 * 1. [Finding.SameKey]: the pinned key is the key of a portal that the phone holds. The address does not matter.
 * 2. [Finding.SameAddressOtherKey]: the address is the address of a portal that the phone holds, with another key.
 * 3. [Finding.SimilarAddress]: the host is the same with another port or path, or is equal after the folding of look-alike
 *    characters (`1` and `l`, `0` and `o`, `rn` and `m`, Cyrillic letters, Punycode decoded), or is a few edits away.
 * 4. [Finding.SimilarName]: the nickname that the user typed looks like the nickname of a portal that the phone holds.
 */
object LookAlike {
    sealed interface Finding {
        object None : Finding

        data class SameKey(val portal: KnownPortal) : Finding

        data class SameAddressOtherKey(val portal: KnownPortal) : Finding

        /** [distance] is the edit distance of the two hosts after folding. 0 means the same host with another port or path, or the same folded host. */
        data class SimilarAddress(val portal: KnownPortal, val distance: Int) : Finding

        data class SimilarName(val portal: KnownPortal) : Finding
    }

    /** What a check needs of an address: the host as a person reads it, the port that applies and the path. */
    data class Parts(val host: String, val port: Int, val path: String) {
        /** True for an IP address: its digits are never "similar", only equal or not. */
        val isIp: Boolean get() = host.startsWith("[") || host.all { it.isDigit() || it == '.' }
    }

    /**
     * Reads [address] (the text of [ServerAddress.http] or any text that [ServerAddress.parse] accepts). The host is lower
     * case and Punycode is decoded. Returns null when the text is not an address.
     */
    fun parts(address: String): Parts? {
        val parsed = ServerAddress.parse(address) ?: return null
        val uri = try { URI(parsed.http) } catch (e: Exception) { return null }
        val host = uri.host ?: return null
        val port = if (uri.port != -1) uri.port else if (parsed.secure) 443 else 80
        return Parts(displayHost(host), port, (uri.rawPath ?: "").trimEnd('/'))
    }

    /** The host as a person reads it: lower case, Punycode (`xn--`) decoded. */
    fun displayHost(host: String): String = IDN.toUnicode(host.lowercase(Locale.ROOT))

    /** True when the host is written in Punycode or holds a character outside ASCII: the screen then marks it. */
    fun isInternational(host: String): Boolean = displayHost(host).any { it.code >= 0x80 }

    /**
     * Checks the portal about to be added: its [newAddress], the compact or display [newKeyFp] and, when the user already
     * typed one, the [newNickname]. [existing] are the portals that the phone holds. Returns the strongest finding.
     */
    fun check(newAddress: String, newKeyFp: String, existing: List<KnownPortal>, newNickname: String? = null): Finding {
        val fp = Fingerprint.parse(newKeyFp)
        if (fp != null) existing.firstOrNull { it.keyFp == fp }?.let { return Finding.SameKey(it) }
        val mine = parts(newAddress)
        if (mine != null) {
            existing.firstOrNull { parts(it.address) == mine }?.let { return Finding.SameAddressOtherKey(it) }
            var best: Finding.SimilarAddress? = null
            for (p in existing) {
                val theirs = parts(p.address) ?: continue
                val d = similarHost(mine, theirs) ?: continue
                if (best == null || d < best.distance) best = Finding.SimilarAddress(p, d)
            }
            if (best != null) return best
        }
        val name = newNickname?.let { PortalNaming.normalize(it) }.orEmpty()
        if (name.isNotEmpty()) {
            existing.firstOrNull { similarName(name, PortalNaming.normalize(it.identity.nickname)) }?.let { return Finding.SimilarName(it) }
        }
        return Finding.None
    }

    /**
     * The distance of two hosts when they look alike, or null. The same host on another port or path gives 0. Two IP
     * addresses never look alike unless they are the same host.
     */
    private fun similarHost(a: Parts, b: Parts): Int? {
        if (a.host == b.host) return 0
        if (a.isIp || b.isIp) return null
        val fa = Confusables.hostSkeleton(a.host)
        val fb = Confusables.hostSkeleton(b.host)
        if (fa == fb) return 0
        val d = minOf(Confusables.distance(fa, fb), Confusables.distance(a.host, b.host))
        return d.takeIf { d <= threshold(minOf(fa.length, fb.length)) }
    }

    private fun similarName(a: String, b: String): Boolean =
        a == b || Confusables.distance(a, b) <= threshold(minOf(a.length, b.length), nameRule = true)

    /**
     * The largest edit distance that counts as "similar". The proposal says 2. A short text has fewer letters to change, so
     * 2 edits turn it into anything: 2 edits are accepted from 10 letters, 1 edit from 5, nothing below (only an equal folded
     * text counts). Names are stricter by one step of length: a name is the choice of the user, not of an attacker.
     */
    private fun threshold(length: Int, nameRule: Boolean = false): Int = when {
        length >= 10 -> if (nameRule) 1 else 2
        length >= 5 -> 1
        else -> 0
    }
}
