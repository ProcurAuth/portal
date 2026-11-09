package app.portalinbox.data

import app.portalinbox.proto.DigestMode
import app.portalinbox.proto.DigestPrefs
import app.portalinbox.proto.Prefs
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.proto.SetPrefsFrame
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PrefsSyncTest {
    private class Memory : PendingPrefs {
        val map = HashMap<Long, Prefs>()
        override fun get(serverId: Long) = map[serverId]
        override fun set(serverId: Long, prefs: Prefs) { map[serverId] = prefs }
        override fun clear(serverId: Long) { map.remove(serverId) }
    }

    private val server = 1L
    private val pending = Memory()
    private val welcome = Make.welcome() // quiet 22:00-07:00 Europe/Paris high rings, daily 09:00
    private val mine = Prefs(QuietPrefs("23:00", "06:00", "Europe/Paris", highRings = false), DigestPrefs(DigestMode.OFF, null, null, null))

    @Test
    fun withNothingPendingTheValuesOfTheWelcomeApply() {
        assertEquals(PrefsSync.Action.None, PrefsSync.onWelcome(pending, server, welcome))
        assertNull(pending.get(server))
        assertEquals(welcome.prefs, PrefsSync.effective(welcome.prefs, pending.get(server)))
    }

    @Test
    fun anEditThatTheServerShowsInItsWelcomeIsConfirmed() {
        pending.set(server, welcome.prefs)
        assertEquals(PrefsSync.Action.None, PrefsSync.onWelcome(pending, server, welcome))
        assertNull(pending.get(server))
    }

    @Test
    fun anEditThatTheServerLacksIsSentAgain() {
        pending.set(server, mine)
        val a = PrefsSync.onWelcome(pending, server, welcome)
        assertEquals(PrefsSync.Action.Send(SetPrefsFrame(mine)), a)
        // The edit stays pending until a welcome shows it.
        assertEquals(mine, pending.get(server))
    }

    @Test
    fun theEditSentAgainIsConfirmedByTheNextWelcome() {
        pending.set(server, mine)
        PrefsSync.onWelcome(pending, server, welcome)
        val later = Make.welcome().let { w -> w.copy(prefs = mine) }
        assertEquals(PrefsSync.Action.None, PrefsSync.onWelcome(pending, server, later))
        assertNull(pending.get(server))
    }

    @Test
    fun aServerWithoutItemsGetsNoFrameAndKeepsTheEdit() {
        pending.set(server, mine)
        val off = Make.welcome("welcome-empty-prefs.json")
        assertEquals(PrefsSync.Action.None, PrefsSync.onWelcome(pending, server, off))
        assertEquals(mine, pending.get(server))
    }

    @Test
    fun theEditOfThePhoneWinsOverTheServerUntilTheServerConfirms() {
        assertEquals(mine, PrefsSync.effective(welcome.prefs, mine))
        assertEquals(welcome.prefs, PrefsSync.effective(welcome.prefs, null))
        assertEquals(Prefs.EMPTY, PrefsSync.effective(null, null))
        assertEquals(mine.quiet, PrefsSync.quiet(welcome.prefs, mine))
        assertEquals(welcome.prefs.quiet, PrefsSync.quiet(welcome.prefs, null))
    }

    @Test
    fun aChangeMadeOnAnotherDeviceReachesThePhoneThroughTheWelcome() {
        // The phone has nothing pending: the web changed the digest, the welcome carries it.
        val changed = Prefs(welcome.prefs.quiet, DigestPrefs(DigestMode.EVERY, null, null, 4))
        val w = welcome.copy(prefs = changed)
        assertEquals(PrefsSync.Action.None, PrefsSync.onWelcome(pending, server, w))
        assertEquals(changed, PrefsSync.effective(w.prefs, pending.get(server)))
    }

    @Test
    fun anEditIsPendingAndGivesTheFrame() {
        val frame = PrefsSync.edit(pending, server, mine)
        assertEquals(SetPrefsFrame(mine), frame)
        assertEquals(mine, pending.get(server))
    }

    @Test
    fun theFrameOfAnEditIsValidAndReadsBack() {
        val frame = PrefsSync.edit(pending, server, mine)
        frame.validate()
        val back = app.portalinbox.proto.ClientFrames.decode(app.portalinbox.proto.StrictJson.compact(frame.toJson()).toByteArray())
        assertEquals(frame, back)
    }

    @Test
    fun theEnginePutsTheWelcomeValuesInTheStorage() {
        val repo = MemoryItemRepository()
        val engine = ItemEngine(repo, ReferenceEffectHasher(), { 1_800_000_000_000L })
        engine.onWelcome(server, welcome)
        assertEquals(welcome.prefs, repo.prefs(server)!!.prefs)
        assertNotNull(repo.prefs(server))
        assertTrue(repo.prefs(server)!!.bodyPurgeAfterMs > 0)
    }

    @Test
    fun theServersAreIndependent() {
        pending.set(1, mine)
        assertNull(pending.get(2))
        assertEquals(PrefsSync.Action.None, PrefsSync.onWelcome(pending, 2, welcome))
        assertEquals(mine, pending.get(1))
    }

    // ------------------------------------------------------------ the portal refuses a value

    @Test
    fun aRefusedEditIsDroppedSoItIsNotSentAgainAtEachWelcome() {
        PrefsSync.edit(pending, server, mine)
        val refusal = PrefsSync.onError(pending, server, "invalid_prefs", "prefs.quiet.tz: unknown zone")
        assertEquals(PrefsSync.Refusal("prefs.quiet.tz: unknown zone"), refusal)
        assertNull(pending.get(server))
        // The next welcome has nothing to send: the values of the portal apply.
        assertEquals(PrefsSync.Action.None, PrefsSync.onWelcome(pending, server, welcome))
        assertEquals(welcome.prefs, PrefsSync.effective(welcome.prefs, pending.get(server)))
    }

    @Test
    fun anotherErrorKeepsTheEdit() {
        PrefsSync.edit(pending, server, mine)
        assertNull(PrefsSync.onError(pending, server, "not_enabled", "items are off"))
        assertNull(PrefsSync.onError(pending, server, "unavailable", "later"))
        assertEquals(mine, pending.get(server))
    }

    @Test
    fun aRefusalDropsTheEditOfThatPortalOnly() {
        pending.set(1, mine)
        pending.set(2, mine)
        PrefsSync.onError(pending, 1, "invalid_prefs", "x")
        assertNull(pending.get(1))
        assertEquals(mine, pending.get(2))
    }
}
