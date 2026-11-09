package app.portalinbox.portal

import app.portalinbox.service.LinkState
import app.portalinbox.service.LinkStatus
import app.portalinbox.testsupport.Portals
import java.time.ZoneOffset
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class OfflineSummaryTest {
    private val hour = 3_600_000L
    private val day = 24 * hour
    private val now = 1_788_000_000_000L
    private val utc = ZoneOffset.UTC
    private val warn = 180_000L

    private fun link(state: LinkState, since: Long = now - 10 * hour) = LinkStatus(state, since, null)

    private fun portal(id: Long, nickname: String, health: PortalHealth, offlineSince: Long? = null, lastSync: Long? = null, isNew: Boolean = false) =
        HomePortal(Portals.identity(id, nickname, id.toInt() - 1), health, isNew, offlineSince, lastSync)

    private fun banners(vararg p: HomePortal, network: Boolean = true) = OfflineSummary.banners(p.toList(), network, now, utc)

    // ------------------------------------------------------------ the health of a link

    @Test
    fun aConnectedPortalIsOk() {
        assertEquals(PortalHealth.OK, OfflineSummary.health(link(LinkState.CONNECTED), warn, now))
    }

    @Test
    fun aPortalThatIsNotConnectedIsOkUntilTheThreshold() {
        assertEquals(PortalHealth.OK, OfflineSummary.health(link(LinkState.DISCONNECTED, now - warn + 1), warn, now))
        assertEquals(PortalHealth.OFFLINE, OfflineSummary.health(link(LinkState.DISCONNECTED, now - warn), warn, now))
        assertEquals(PortalHealth.OFFLINE, OfflineSummary.health(link(LinkState.CONNECTING, now - 5 * hour), warn, now))
    }

    @Test
    fun aKeyProblemShowsAtOnce() {
        assertEquals(PortalHealth.KEY_CHANGED, OfflineSummary.health(link(LinkState.KEY_CHANGED, now), warn, now))
        assertEquals(PortalHealth.RE_ENROLL, OfflineSummary.health(link(LinkState.REENROLL, now), warn, now))
        assertTrue(PortalHealth.KEY_CHANGED.isDanger && PortalHealth.RE_ENROLL.isDanger)
        assertFalse(PortalHealth.OFFLINE.isDanger)
        assertFalse(PortalHealth.OK.isDown)
    }

    @Test
    fun theOfflineTimeCountsFromTheLastSyncWhenTheSyncIsOlderThanTheLinkState() {
        val s = Portals.server(1, "Homelab", 3).copy(lastSyncMs = now - 3 * hour)
        val list = HomePortal.build(listOf(s), { link(LinkState.DISCONNECTED, now - 10 * 60_000) }, now)
        assertEquals(now - 3 * hour, list.single().offlineSinceMs)
        // A sync time after the state change (clock jump): the state time is used.
        val s2 = s.copy(lastSyncMs = now)
        assertEquals(now - 10 * 60_000, HomePortal.build(listOf(s2), { link(LinkState.DISCONNECTED, now - 10 * 60_000) }, now).single().offlineSinceMs)
        // A portal that is fine has no offline time.
        assertNull(HomePortal.build(listOf(s), { link(LinkState.CONNECTED) }, now).single().offlineSinceMs)
    }

    @Test
    fun thePortalsComeInTheOrderOfTheUserAndTheNewMarkLastsADay() {
        val a = Portals.server(1, "Production", 0).copy(sortIndex = 2, addedAtMs = now - 2 * day)
        val b = Portals.server(2, "Staging", 1).copy(sortIndex = 1, addedAtMs = now - hour)
        val list = HomePortal.build(listOf(a, b), { link(LinkState.CONNECTED) }, now)
        assertEquals(listOf("Staging", "Production"), list.map { it.identity.nickname })
        assertEquals(listOf(true, false), list.map { it.isNew })
    }

    // ------------------------------------------------------------ the banners

    @Test
    fun noPortalGivesNoBanner() {
        assertTrue(banners().isEmpty())
        assertTrue(banners(network = false).isEmpty())
    }

    @Test
    fun portalsThatAreFineGiveNoBanner() {
        assertTrue(banners(portal(1, "Production", PortalHealth.OK)).isEmpty())
    }

    @Test
    fun twoPortalsOfflineGiveTwoBanners() {
        val list = banners(
            portal(1, "Homelab", PortalHealth.OFFLINE, offlineSince = now - 3 * hour),
            portal(2, "Staging", PortalHealth.OFFLINE, offlineSince = now - 20 * 60_000),
        )
        assertEquals(2, list.size)
        assertEquals(listOf(1L, 2L), list.map { it.portalId })
        assertTrue(list.all { it.kind == OfflineSummary.Kind.OFFLINE && it.severity == OfflineSummary.Severity.WARNING && it.action == null })
        assertTrue(list[0].title.startsWith("Homelab is offline since "))
        assertTrue(list[0].title.endsWith("(3 h)."))
        assertTrue(list[1].title.endsWith("(20 min)."))
        assertEquals("Its requests may be out of date. Your answers wait on the phone.", list[0].text)
    }

    @Test
    fun aKeyChangeGivesTheDangerBannerWithAFixButton() {
        val b = banners(portal(3, "Client A", PortalHealth.KEY_CHANGED)).single()
        assertEquals(OfflineSummary.Kind.KEY_CHANGED, b.kind)
        assertEquals(OfflineSummary.Severity.DANGER, b.severity)
        assertEquals("Client A: key check failed.", b.title)
        assertEquals("Not connected. Nothing is sent to it.", b.text)
        assertEquals("Fix", b.action)
        assertEquals(3L, b.portalId)
    }

    @Test
    fun aPortalThatMustEnrollAgainGivesADangerBannerToo() {
        val b = banners(portal(3, "Client A", PortalHealth.RE_ENROLL)).single()
        assertEquals(OfflineSummary.Kind.RE_ENROLL, b.kind)
        assertEquals(OfflineSummary.Severity.DANGER, b.severity)
        assertEquals("Fix", b.action)
    }

    @Test
    fun dangerBannersComeBeforeWarnings() {
        val list = banners(
            portal(1, "Homelab", PortalHealth.OFFLINE, offlineSince = now - hour),
            portal(2, "Client A", PortalHealth.KEY_CHANGED),
        )
        assertEquals(listOf(OfflineSummary.Kind.KEY_CHANGED, OfflineSummary.Kind.OFFLINE), list.map { it.kind })
    }

    @Test
    fun withNoNetworkOneBannerReplacesTheBannerOfEachOfflinePortal() {
        val list = banners(
            portal(1, "Homelab", PortalHealth.OFFLINE, offlineSince = now - hour),
            portal(2, "Staging", PortalHealth.OFFLINE, offlineSince = now - hour),
            portal(3, "Production", PortalHealth.OK),
            network = false,
        )
        val one = list.single()
        assertEquals(OfflineSummary.Kind.NO_NETWORK, one.kind)
        assertNull(one.portalId)
        assertEquals("No network.", one.title)
        assertEquals("All portals are paused. Your answers wait on the phone.", one.text)
    }

    @Test
    fun withNoNetworkAKeyProblemStillShowsBecauseOnlyTheUserCanFixIt() {
        val list = banners(portal(1, "Homelab", PortalHealth.OFFLINE, offlineSince = now - hour), portal(2, "Client A", PortalHealth.KEY_CHANGED), network = false)
        assertEquals(listOf(OfflineSummary.Kind.KEY_CHANGED, OfflineSummary.Kind.NO_NETWORK), list.map { it.kind })
    }

    @Test
    fun moreThanThreeProblemsFoldIntoOneLine() {
        val ps = (1L..5L).map { portal(it, "P$it", PortalHealth.OFFLINE, offlineSince = now - hour) }
        val list = OfflineSummary.banners(ps, true, now, utc)
        assertEquals(OfflineSummary.MAX_BANNERS, list.size)
        assertEquals(OfflineSummary.Kind.MORE, list.last().kind)
        assertEquals("3 more portals have a problem.", list.last().title)
        assertEquals("Show", list.last().action)
        assertEquals(listOf(1L, 2L), list.take(2).map { it.portalId })
    }

    @Test
    fun aHiddenCharacterInANameCannotReachABanner() {
        val b = banners(portal(1, "Pr‮od", PortalHealth.KEY_CHANGED)).single()
        assertFalse(b.title.contains('‮'))
    }

    // ------------------------------------------------------------ the last line of a row

    @Test
    fun aFineRowKeepsItsUsualLine() {
        assertNull(OfflineSummary.rowReason(portal(1, "Production", PortalHealth.OK), now))
    }

    @Test
    fun anOfflineRowSaysHowLongAndThatTheAnswerWaits() {
        assertEquals("Offline 3 h · answer waits", OfflineSummary.rowReason(portal(1, "Homelab", PortalHealth.OFFLINE, offlineSince = now - 3 * hour), now))
    }

    @Test
    fun aRowOfAPortalWithAKeyProblemSaysItIsNotConnected() {
        assertEquals(
            "Not connected · last sync 3 d ago",
            OfflineSummary.rowReason(portal(1, "Client A", PortalHealth.KEY_CHANGED, lastSync = now - 3 * day), now),
        )
        assertEquals("Not connected · never synced", OfflineSummary.rowReason(portal(1, "Client A", PortalHealth.RE_ENROLL), now))
    }
}

class LinkStateTest {
    @Test
    fun connectingAndDisconnectedAreOnePeriodWithoutALink() {
        assertTrue(LinkState.CONNECTING.notConnected)
        assertTrue(LinkState.DISCONNECTED.notConnected)
        assertFalse(LinkState.CONNECTED.notConnected)
        assertFalse(LinkState.KEY_CHANGED.notConnected)
        assertFalse(LinkState.REENROLL.notConnected)
    }
}
