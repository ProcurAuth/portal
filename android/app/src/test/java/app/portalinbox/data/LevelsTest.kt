package app.portalinbox.data

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class LevelsTest {
    private val direct = Levels.DIRECT_ID

    @Test
    fun theConstantsAreTheOnesOfTheServer() {
        assertEquals("@direct", Levels.DIRECT_ID)
        assertEquals("Direct", Levels.DIRECT_NAME)
        assertEquals("ring", Levels.DIRECT_DEFAULT)
        assertTrue(Levels.isDirect("@direct"))
        assertFalse(Levels.isDirect("direct"))
        assertFalse(Levels.isDirect("ops"))
    }

    @Test
    fun aDirectAlertIsRingByDefaultAndNeverAlarm() {
        assertEquals("ring", Levels.alertAction(direct, emptyMap()))
        assertEquals("ring", Levels.alertAction(direct, mapOf("ops" to "alarm")))
    }

    @Test
    fun theUserCanChangeTheLevelOfDirect() {
        for (level in listOf("notify", "ring", "alarm")) {
            assertEquals(level, Levels.alertAction(direct, mapOf(direct to level)))
        }
    }

    @Test
    fun directOffIsFinalAndSilent() {
        assertEquals("off", Levels.alertAction(direct, mapOf(direct to "off")))
        assertNull(Levels.shown(direct, mapOf(direct to "off")))
    }

    @Test
    fun aBadStoredLevelOfDirectFallsBackToRing() {
        assertEquals("ring", Levels.alertAction(direct, mapOf(direct to "bogus")))
        assertEquals("ring", Levels.shown(direct, mapOf(direct to "bogus")))
    }

    @Test
    fun anUnknownGroupStaysNotification() {
        assertEquals("notify", Levels.alertAction("unknown", emptyMap()))
        assertEquals("notify", Levels.alertAction("ops", mapOf("ops" to "off")))
        assertEquals("alarm", Levels.alertAction("ops", mapOf("ops" to "alarm")))
        assertEquals("ring", Levels.alertAction("ops", mapOf("ops" to "ring")))
    }

    @Test
    fun theSettingsScreenShowsDirectEvenWithoutAlertOrGroup() {
        val list = Levels.withDirect(emptyList(), "desc")
        assertEquals(listOf(Group("@direct", "Direct", "desc")), list)
        val ops = Group("ops", "Operations", "")
        val both = Levels.withDirect(listOf(ops), "desc")
        assertEquals(listOf("ops", "@direct"), both.map { it.id })
        assertEquals("ring", Levels.shown(direct, emptyMap()))
        assertNull(Levels.shown("ops", emptyMap()))
    }

    @Test
    fun aGroupOfTheServerWithTheIdDirectIsDropped() {
        val list = Levels.withDirect(listOf(Group("@direct", "Fake", "x")), "desc")
        assertEquals(listOf(Group("@direct", "Direct", "desc")), list)
    }

    @Test
    fun theWelcomeNeverSubscribesNorUnsubscribesDirect() {
        // Ring stored: not subscribed on the server, still no subscribe frame.
        var plan = Levels.reconcile(mapOf(direct to "ring", "ops" to "notify"), setOf("ops"), emptySet())
        assertEquals(listOf("ops"), plan.reconcile.subscribe)
        assertTrue(plan.reconcile.unsubscribe.isEmpty())
        assertEquals("ring", plan.levels[direct])
        // Off stored: no unsubscribe frame, even if the server lists the id.
        plan = Levels.reconcile(mapOf(direct to "off"), setOf("ops"), setOf(direct))
        assertTrue(plan.reconcile.subscribe.isEmpty())
        assertTrue(plan.reconcile.unsubscribe.isEmpty())
        assertEquals("off", plan.levels[direct])
        // The server lists the id and a group: only the group gets a default level.
        plan = Levels.reconcile(emptyMap(), setOf("ops", direct), setOf("ops", direct))
        assertEquals(mapOf("ops" to "notify"), plan.levels)
        assertEquals(Reconcile(emptyList(), emptyList()), plan.reconcile)
    }

    @Test
    fun theWelcomeKeepsTheLevelOfDirectWhenNoGroupExists() {
        val plan = Levels.reconcile(mapOf(direct to "alarm", "gone" to "ring"), emptySet(), emptySet())
        assertEquals(mapOf(direct to "alarm"), plan.levels)
        assertEquals(Reconcile(emptyList(), emptyList()), plan.reconcile)
    }

    @Test
    fun theWelcomeStillReconcilesTheGroupsOfTheServer() {
        val plan = Levels.reconcile(
            mapOf("a" to "ring", "b" to "off", "c" to "alarm"), setOf("a", "b", "c", "d"), setOf("b", "c", "d"),
        )
        assertEquals(listOf("a"), plan.reconcile.subscribe)
        assertEquals(listOf("b"), plan.reconcile.unsubscribe)
        assertEquals("notify", plan.levels["d"])
    }

    @Test
    fun aSubscriptionsMessageNeverTurnsDirectOff() {
        val local = mapOf(direct to "ring", "a" to "ring", "b" to "notify", "c" to "off")
        val lost = Levels.lostOnServer(local, setOf("b"), emptyMap(), 10_000L, 3_000L)
        assertEquals(listOf("a"), lost)
        // A group asked a moment ago is left alone.
        assertTrue(Levels.lostOnServer(local, setOf("b"), mapOf("a" to 9_000L), 10_000L, 3_000L).isEmpty())
    }

    @Test
    fun noServerFrameForDirect() {
        assertFalse(Levels.isServerGroup(direct))
        assertTrue(Levels.isServerGroup("ops"))
    }

    @Test
    fun aDirectFrameParsesAsAnAlertOfTheGroupDirect() {
        val o = JSONObject()
            .put("t", "alert").put("alert_id", "ab".repeat(16)).put("kind", "alert")
            .put("group", "@direct").put("group_name", "Direct").put("title", "call me")
            .put("created_at_ms", 1_700_000_000_000L)
        val f = AlertFrame.parse(o)
        assertEquals(Levels.DIRECT_ID, f.group)
        assertEquals(Levels.DIRECT_NAME, f.groupName)
        assertEquals("ring", Levels.alertAction(f.group, emptyMap()))
    }
}
