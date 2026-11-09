package app.portalinbox.testsupport

import app.portalinbox.alert.Attention
import app.portalinbox.alert.AttentionCenter
import app.portalinbox.alert.DigestPlan
import app.portalinbox.data.Alert
import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.proto.Urgency

/** A sink that records what the one notification does. Posting shows it, cancelling removes it, [dismiss] is the owner swiping it away. */
class FakeSink : AttentionCenter.Sink {
    val posts = ArrayList<Attention.Plan>()
    var cancels = 0
    val played = ArrayList<Attention.Sound>()
    var shown = false

    override fun isShown() = shown

    override fun post(plan: Attention.Plan) {
        posts.add(plan)
        shown = true
    }

    override fun cancel() {
        cancels++
        shown = false
    }

    override fun play(sound: Attention.Sound) {
        played.add(sound)
    }

    fun dismiss() {
        shown = false
    }

    /** Every (tag, id) pair that the center ever used. */
    val keys: Set<Pair<String, Int>> get() = posts.map { it.tag to it.id }.toSet()
}

/** Items, alerts and portals for the tests of the one notification. */
class AttentionWorld(val now: Long = 1_788_000_100_000L) {
    val repo = MemoryItemRepository()
    val engine = ItemEngine(repo, ReferenceEffectHasher(), { now })
    private var n = 0x100

    val production = Portals.identity(1, "Production", 0)
    val homelab = Portals.identity(2, "Homelab", 4)

    fun portal(
        identity: PortalIdentity,
        hide: Boolean = false,
        muted: Boolean = false,
        quiet: QuietPrefs? = null,
        down: Attention.Down? = null,
    ) = Attention.Portal(identity, hide, muted, quiet, now, down)

    private fun next(): Int = n++

    fun approval(
        server: Long = 1, urgency: Urgency = Urgency.NORMAL, quiet: Boolean = false, ageMs: Long = 60_000, rid: String = Make.hex(next()),
    ): Item {
        engine.onRequest(
            server,
            Make.approval(rid, Make.hex(next()), createdAtMs = now - ageMs) { it.put("urgency", urgency.wire).put("quiet", quiet) },
        )
        return repo.item(server, rid)!!
    }

    fun question(server: Long = 1, urgency: Urgency = Urgency.NORMAL, quiet: Boolean = false, ageMs: Long = 60_000, rid: String = Make.hex(next())): Item {
        engine.onRequest(
            server,
            Make.question(rid, Make.hex(next()), createdAtMs = now - ageMs) { it.put("urgency", urgency.wire).put("quiet", quiet) },
        )
        return repo.item(server, rid)!!
    }

    /** The item as the phone shows it after the first notification. */
    fun seen(item: Item): Item {
        repo.markNotified(item.serverId, item.requestId)
        return repo.item(item.serverId, item.requestId)!!
    }

    fun alert(
        server: Long = 1, id: String = "a${next()}", action: String = "notify", ageMs: Long = 60_000, notified: Boolean = false,
        stoppedAtMs: Long? = null, readAtMs: Long? = null,
    ) = Alert(server, id, "infra", "infra", "alert", "Disk full", now - ageMs, now, action, notified, stoppedAtMs, readAtMs)

    fun input(
        portals: List<Attention.Portal> = listOf(portal(production)),
        items: List<Item> = emptyList(),
        alerts: List<Alert> = emptyList(),
        replies: List<Attention.Reply> = emptyList(),
        network: Boolean = true,
        digests: List<DigestPlan.Waiting> = emptyList(),
    ) = Attention.Input(portals, items, alerts, replies, now, network, digests)

    /** What a digest of [server] says still waits: [count] items, the oldest [ageMs] old on the clock of the portal. */
    fun waiting(server: Long, count: Long, ageMs: Long, id: String = "dg$server") = DigestPlan.Waiting(server, id, count, now - ageMs)
}
