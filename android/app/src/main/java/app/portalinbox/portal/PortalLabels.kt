package app.portalinbox.portal

import app.portalinbox.data.Server
import app.portalinbox.ui.VisibleText

/**
 * The texts of the identity banner and of the footer of the approval screen. Pure Kotlin.
 *
 * The nickname, the color and the key come from the phone. The login is what the portal says about the account, so it goes
 * through [VisibleText.escapeLine]. The host comes from the address that the user enrolled.
 */
object PortalLabels {
    /** The host of the address: `portal.example.org` for `https://portal.example.org:8443/x`. The whole text when it has no host. */
    fun hostOf(address: String): String {
        val noScheme = address.substringAfter("://", address)
        val host = noScheme.substringBefore('/').substringBefore('?').substringBefore('#').substringAfterLast('@')
        // An IPv6 host keeps its brackets and its colons. Any other host loses the port.
        val bare = if (host.startsWith("[")) host.substringBefore(']') + "]" else host.substringBefore(':')
        return bare.ifEmpty { address }
    }

    /** "portal.example.org · alice". */
    fun hostAndLogin(server: Server): String = hostOf(server.address) + " · " + VisibleText.escapeLine(server.userName)

    /** "key a3f9 07c2": the first two groups of the fingerprint. The banner shows this. */
    fun bannerKey(identity: PortalIdentity): String = "key " + identity.shortFingerprint()

    /** "Production · key a3f9 07c2 5be1 d84a 9c30": the whole fingerprint. The footer shows this and stays on screen. */
    fun footer(identity: PortalIdentity): String = "${VisibleText.escapeLine(identity.nickname)} · key ${identity.fingerprint}"

    /** The label of the clipboard when the owner copies the command: "Production: command". */
    fun clipboardLabel(identity: PortalIdentity): String = "${VisibleText.escapeLine(identity.nickname)}: command"
}
