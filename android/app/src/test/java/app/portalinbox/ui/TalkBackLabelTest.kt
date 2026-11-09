package app.portalinbox.ui

import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.data.Submit
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.ZoneOffset

class TalkBackLabelTest {
    private val server = 1L
    private val day = 86_400_000L
    private val created = 1_788_000_000_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { created }, { "m-1" })
    private val rid = Make.hex(0xb)

    private fun approval(edit: (org.json.JSONObject) -> Unit = {}): Item {
        engine.onRequest(server, Make.approval(rid, Make.hex(1), createdAtMs = created, edit = edit))
        return repo.item(server, rid)!!
    }

    @Test
    fun anApprovalRowSaysKindCodeSenderServiceAgesAndState() {
        val label = TalkBackLabels.itemRow(approval { it.put("agent_seen_at_ms", created + 6 * day - 3 * 60_000) }, created + 6 * day)
        assertEquals(
            "Approval K7QF from claude-laptop, datastore, asked 6 days ago, agent active 3 minutes ago, waiting for you",
            label,
        )
    }

    @Test
    fun aQuestionRowSaysTheChoicesAndTheAgeInFullWords() {
        engine.onRequest(server, Make.question(rid, Make.hex(2), createdAtMs = created) { it.put("agent_seen_at_ms", created + 6 * day - 180_000) })
        val label = TalkBackLabels.itemRow(repo.item(server, rid)!!, created + 6 * day)
        assertEquals(
            "Question from the agent claude-laptop, Purge the staging data of November?, asked 6 days ago, " +
                "agent active 3 minutes ago, waiting for you, 2 options",
            label,
        )
    }

    @Test
    fun noShortFormAppearsInALabel() {
        val label = TalkBackLabels.itemRow(approval(), created + 3 * 3_600_000)
        for (short in listOf(" min ", " h ", " d ", "<1")) assertFalse(label, label.contains(short))
    }

    @Test
    fun aRowAfterAReplySaysThatTheAgentMustAnswer() {
        approval()
        val e = (engine.reply(server, rid, "why") as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted", "replied"))
        assertTrue(TalkBackLabels.itemRow(repo.item(server, rid)!!, created + 60_000).endsWith("waiting for the agent to answer"))
    }

    @Test
    fun anInvalidApprovalSaysSo() {
        val bad = approval { it.getJSONObject("approval").put("effect", "sha256:" + "0".repeat(64)) }
        assertTrue(TalkBackLabels.itemRow(bad, created + 60_000).endsWith("invalid request"))
    }

    @Test
    fun aHiddenCharacterInTheSenderIsSpokenAsItsMark() {
        val label = TalkBackLabels.itemRow(approval { it.getJSONObject("from").put("label", "ops‮agent") }, created + 60_000)
        assertTrue(label, label.contains("from ops<U+202E>agent"))
    }

    @Test
    fun theLabelOfATurnIntroducesTheAgentTextAsNotChecked() {
        val agent = ThreadState.Entry.Agent(1, 0, "14:02", "Hello", null, null)
        assertEquals("ops-agent, 14:02, not checked. Hello", TalkBackLabels.turn(agent, "ops-agent", "Hello"))
        val mine = ThreadState.Entry.User(null, 0, "14:09", "Why?", ThreadState.Delivery.QUEUED, "Saved. It goes out when the phone is online.", "m")
        assertEquals("You, 14:09. Why?. Saved. It goes out when the phone is online.", TalkBackLabels.turn(mine, "ops-agent", "Why?"))
    }

    private fun screen(): ThreadState.Screen = ThreadState.of(
        ThreadState.Input(repo.item(server, rid)!!, repo.turns(server, rid), repo.outboxOf(server, rid), created + 4 * 60_000, 0, ZoneOffset.UTC),
    )

    @Test
    fun theFieldAndTheSendButtonSayWhyTheyAreBlocked() {
        approval()
        val open = screen().composer
        assertEquals("Write to the agent", TalkBackLabels.composer(open))
        assertEquals("Send. Write a message first.", TalkBackLabels.sendButton(open, hasText = false))
        assertEquals("Send the message to the agent", TalkBackLabels.sendButton(open, hasText = true))
        engine.onMessage(server, Make.message(rid, Make.hex(5), 1, user = true))
        val blocked = screen().composer
        assertEquals("Message field. Blocked. The agent is writing its answer. You can write again when it replies.", TalkBackLabels.composer(blocked))
        assertEquals("Send. Not available now.", TalkBackLabels.sendButton(blocked, hasText = true))
    }

    @Test
    fun theHeaderOfTheThreadIsOneSentenceWithTheState() {
        approval { it.put("agent_seen_at_ms", created + 60_000) }
        val s = screen()
        val text = TalkBackLabels.threadHeader(s)
        assertTrue(text, text.startsWith("ClickHouse write. claude-laptop · key of alice · datastore. agent active 3 minutes ago, your turn"))
        assertEquals("claude-laptop is writing its answer", TalkBackLabels.writing("claude-laptop"))
    }

    @Test
    fun everyButtonOfTheScreenHasATouchTargetOf48dpOrMore() {
        // The sizes are constants of the Activities: the check reads the source of the two new screens.
        val sources = listOf("ThreadActivity.kt", "PrefsActivity.kt").map { java.io.File("src/main/java/app/portalinbox/ui/$it").readText() }
        for (src in sources) {
            for (m in Regex("""minHeight = dp\((\d+)\)""").findAll(src)) assertTrue(m.value, m.groupValues[1].toInt() >= 48)
        }
        assertTrue(sources[0].contains("dp(52), dp(52)"))
    }
}
