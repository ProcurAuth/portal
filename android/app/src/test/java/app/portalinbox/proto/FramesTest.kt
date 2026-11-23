package app.portalinbox.proto

import app.portalinbox.testsupport.Fixtures
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/** The golden frames of `crates/inbox-core/tests/fixtures/frames`: the Kotlin parsers read the same files as Rust. */
class FramesTest {
    private fun same(expected: JSONObject, actual: JSONObject, what: String) =
        assertTrue("$what: the frame does not round trip\n${StrictJson.compact(expected)}\n${StrictJson.compact(actual)}", expected.similar(actual))

    private fun refused(block: () -> Unit) {
        assertThrows(FrameException::class.java) { block() }
    }

    // ------------------------------------------------------------ golden, valid

    @Test
    fun everyGoldenServerFrameParsesAndWritesBackTheSameJson() {
        val files = Fixtures.frames("server")
        assertTrue(files.size >= 15)
        for (f in files) {
            val frame = try {
                ServerFrames.decode(f.text)
            } catch (e: FrameException) {
                throw AssertionError("server/${f.name}: ${e.message}")
            }
            same(f.json, frame.toJson(), "server/${f.name}")
            // The bytes that the server would send read back to the same frame.
            val again = ServerFrames.decode(StrictJson.compact(frame.toJson()).toByteArray())
            same(f.json, again.toJson(), "server/${f.name} (second read)")
        }
    }

    @Test
    fun everyGoldenClientFrameParsesAndWritesBackTheSameBytes() {
        val files = Fixtures.frames("client")
        assertTrue(files.size >= 12)
        for (f in files) {
            val frame = try {
                ClientFrames.decode(f.text)
            } catch (e: FrameException) {
                throw AssertionError("client/${f.name}: ${e.message}")
            }
            same(f.json, frame.toJson(), "client/${f.name}")
            val wire = ClientFrames.encode(frame)
            assertEquals("client/${f.name}", frame, ClientFrames.decode(wire))
            assertTrue(wire.size <= MAX_PLAINTEXT)
        }
    }

    @Test
    fun theGoldenFilesCoverEveryFrameInBothDirections() {
        assertEquals(
            setOf(
                "welcome", "alert", "request", "message", "settled", "digest", "answer_result", "agent", "agent_result",
                "subscriptions", "pong", "error",
            ),
            Fixtures.frames("server").map { it.tag }.toSet(),
        )
        assertEquals(
            setOf("ready", "ack", "subscribe", "unsubscribe", "ping", "answer", "reply", "refresh", "sync", "set_prefs", "agent_do"),
            Fixtures.frames("client").map { it.tag }.toSet(),
        )
        assertTrue(Fixtures.frames("invalid/server").map { it.tag }.toSet().size >= 7)
        assertTrue(Fixtures.frames("invalid/client").map { it.tag }.toSet().size >= 5)
    }

    @Test
    fun theGoldenApprovalReadsToTheTypedValues() {
        val r = Fixtures.server("request-approval.json") as RequestFrame
        assertEquals(RequestKind.APPROVAL, r.kind)
        assertEquals("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", r.requestId)
        assertEquals("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", r.id)
        assertEquals(0L, r.seq)
        assertEquals(
            Sender(
                "Billing bot", "claude-laptop", "alice", Via.MCP, Interaction.CONVERSATIONAL,
                "a1".repeat(16), AgentKind.AGENT, AgentState.KNOWN,
            ),
            r.from,
        )
        assertEquals("billing-bot", r.agentLabel)
        assertEquals(Urgency.HIGH, r.urgency)
        assertFalse(r.quiet)
        assertEquals(LeaseMode.LEASE, r.leaseMode)
        assertEquals(1788000180000L, r.leaseUntilMs)
        assertNull(r.untilAtMs)
        assertTrue(r.allowCustom && r.allowAskBack)
        assertNull(r.question)
        val a = r.approval!!
        assertEquals("K7QF", a.code)
        assertEquals("datastore", a.service)
        assertEquals("data.write", a.action)
        assertEquals("host:prod/database:analytics", a.resource)
        assertEquals("key:claude-laptop", a.subject)
        assertEquals("sha256:9e07172b785fdac847ecde1c13a6245449c0a4a691454245dcf155b457014290", a.effect)
        assertEquals("INSERT INTO analytics.events SELECT 1", a.op.getString("sql"))
        assertEquals("/sql", a.view.body)
        assertEquals("SQL", a.view.bodyLabel)
        assertEquals(listOf("Host" to "/host", "Database" to "/database", "Runs as" to "/ch_user"), a.view.fields)
        assertEquals("The migration needs this row. The dry run passed.", a.explanation)
        assertEquals(Hint("approve", null, "system_one"), a.hint)
        assertEquals(Previous(1787999000000L, true), a.previous)
        assertEquals(Follows("M3XA", Intent.CHANGE_REQUEST), a.follows)
    }

    @Test
    fun theGoldenQuestionsReadToTheTypedValues() {
        val choice = Fixtures.server("request-question-choice.json") as RequestFrame
        val q = choice.question!!
        assertEquals("Purge the staging data of November?", q.title)
        assertEquals(InputType.CHOICE, q.input.type)
        assertEquals(listOf("go", "wait"), q.input.options.map { it.id })
        assertEquals("Prod, 3 pods", q.input.options[0].description)
        assertEquals(Command("DELETE FROM staging.events WHERE ts < '2026-11-01';", "SQL"), q.command)
        assertEquals(Hint("go", 0.62, "system_one"), q.hint)
        assertEquals(1788000000000L, q.asOfMs)
        assertEquals("dddddddddddddddddddddddddddddddd", choice.previousId)
        assertNull(choice.approval)

        val oneShot = Fixtures.server("request-question-confirm-one-shot.json") as RequestFrame
        assertEquals(Interaction.ONE_SHOT, oneShot.from.interaction)
        assertEquals(Via.API, oneShot.from.via)
        assertFalse(oneShot.allowCustom || oneShot.allowAskBack)
        assertEquals(InputType.CONFIRM, oneShot.question!!.input.type)

        val until = Fixtures.server("request-question-text-until.json") as RequestFrame
        assertEquals(LeaseMode.UNTIL, until.leaseMode)
        assertNull(until.leaseUntilMs)
        assertEquals(1788600000000L, until.untilAtMs)
        assertTrue(until.quiet)
        assertEquals("branch name", until.question!!.input.placeholder)
        assertEquals("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", until.supersedes)
    }

    @Test
    fun theOtherGoldenServerFramesReadToTheTypedValues() {
        val w = Fixtures.server("welcome.json") as WelcomeFrame
        assertEquals("Prod", w.serverName)
        assertEquals(listOf(GroupInfo("infra", "Infra", "Servers")), w.groups)
        assertEquals(listOf("infra"), w.subscriptions)
        assertEquals(QuietPrefs("22:00", "07:00", "Europe/Paris", true), w.prefs.quiet)
        assertEquals(DigestPrefs(DigestMode.DAILY, "09:00", "Europe/Paris", null), w.prefs.digest)
        assertEquals(2592000000L, w.bodyPurgeAfterMs)
        assertTrue(w.itemsEnabled)
        val empty = Fixtures.server("welcome-empty-prefs.json") as WelcomeFrame
        assertEquals(Prefs.EMPTY, empty.prefs)
        assertFalse(empty.itemsEnabled)

        val m = Fixtures.server("message-agent.json") as MessageFrame
        assertEquals(Author.AGENT, m.by)
        assertEquals(1L, m.seq)
        assertEquals(Command("SELECT * FROM analytics.events WHERE id = 7", "SQL"), m.command)
        assertNull(m.text)
        val u = Fixtures.server("message-user.json") as MessageFrame
        assertEquals("Why not use the staging database?", u.text)
        assertNull(u.explanation)

        val s = Fixtures.server("settled-answered.json") as SettledFrame
        assertEquals(ItemState.ANSWERED, s.state)
        assertEquals(Decision.APPROVE, s.decision)
        assertEquals(SettledBy(Channel.PHONE, "Pixel 9"), s.by)
        val sup = Fixtures.server("settled-superseded.json") as SettledFrame
        assertEquals("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", sup.supersededBy)
        val ab = Fixtures.server("settled-abandoned.json") as SettledFrame
        assertEquals("lease_lapsed", ab.reason)

        val d = Fixtures.server("digest.json") as DigestFrame
        assertEquals(3L, d.count)
        assertEquals(3, d.ids.size)
        assertEquals(1787400000000L, d.oldestCreatedAtMs)

        val r = Fixtures.server("answer-result.json") as AnswerResultFrame
        assertEquals(AnswerStatus.ALREADY_ANSWERED, r.status)
        assertEquals(ItemState.ANSWERED, r.state)
        val mm = Fixtures.server("answer-result-effect-mismatch.json") as AnswerResultFrame
        assertEquals(AnswerStatus.EFFECT_MISMATCH, mm.status)
        assertNull(mm.state)

        assertTrue((Fixtures.server("error-not-enabled.json") as ErrorFrame).isNotEnabled)
        assertEquals(7L, (Fixtures.server("pong.json") as PongFrame).nonce)
        assertEquals(listOf("infra"), (Fixtures.server("subscriptions.json") as SubscriptionsFrame).subscriptions)
        val a = Fixtures.server("alert.json") as ServerAlertFrame
        assertEquals("Disk full on a node", a.title)
        assertEquals("infra", a.group)
    }

    @Test
    fun theGoldenClientFramesReadToTheTypedValues() {
        fun client(name: String) = ClientFrames.decode(Fixtures.golden("client", name).text)
        assertEquals(AckFrame("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), client("ack-request.json"))
        val approve = client("answer-approve.json") as AnswerFrame
        assertEquals(Action.ACCEPT, approve.action)
        assertTrue(Check.isEffect(approve.shownEffect!!))
        val decline = client("answer-decline-change-request.json") as AnswerFrame
        assertEquals(Action.DECLINE, decline.action)
        assertEquals(Intent.CHANGE_REQUEST, decline.intent)
        assertEquals("Use the staging database.", decline.text)
        assertEquals(listOf("go"), (client("answer-choice.json") as AnswerFrame).choice)
        assertEquals(2, (client("sync.json") as SyncFrame).open.size)
        assertEquals(0, (client("sync-empty.json") as SyncFrame).open.size)
        assertEquals(DigestMode.EVERY, (client("set-prefs.json") as SetPrefsFrame).prefs.digest!!.mode)
        assertEquals(Prefs.EMPTY, (client("set-prefs-empty.json") as SetPrefsFrame).prefs)
        assertEquals(ReadyFrame, client("ready.json"))
    }

    // ------------------------------------------------------------ golden, invalid

    @Test
    fun everyInvalidServerFrameIsRefused() {
        val files = Fixtures.frames("invalid/server")
        assertTrue(files.size >= 40)
        for (f in files) {
            try {
                ServerFrames.decode(f.text)
                throw AssertionError("invalid/server/${f.name} was accepted")
            } catch (e: FrameException) {
                // Refused with the error of the protocol, not with another exception.
            }
        }
    }

    @Test
    fun everyInvalidClientFrameIsRefused() {
        val files = Fixtures.frames("invalid/client")
        assertTrue(files.size >= 20)
        for (f in files) {
            try {
                ClientFrames.decode(f.text)
                throw AssertionError("invalid/client/${f.name} was accepted")
            } catch (e: FrameException) {
            }
        }
    }

    @Test
    fun theOldNameOfTheReceiptIsNotAccepted() {
        // Protocol 4 sent `alert_id` in the receipt. Protocol 5 has `id` only. No alias.
        refused { ClientFrames.decode("""{"t":"ack","alert_id":"${"0".repeat(32)}","stage":"received"}""") }
        val wire = String(ClientFrames.encode(AckFrame("0".repeat(32))))
        assertTrue(wire, wire.contains("\"id\":\"${"0".repeat(32)}\""))
        assertFalse(wire.contains("alert_id"))
    }

    // ------------------------------------------------------------ closed schema

    /** Adds the member `zz` to each object of a frame, one object at a time. `op` takes any member. */
    private fun withExtraMember(root: JSONObject): List<Pair<String, JSONObject>> {
        val out = ArrayList<Pair<String, JSONObject>>()
        fun walk(v: Any?, path: List<String>) {
            when (v) {
                is JSONObject -> {
                    if (path.lastOrNull() != "op") {
                        val copy = StrictJson.parseObject(StrictJson.compact(root))
                        var cur: Any = copy
                        for (p in path) cur = if (cur is JSONArray) cur.get(p.toInt()) else (cur as JSONObject).get(p)
                        (cur as JSONObject).put("zz", 1)
                        out += path.joinToString("/") to copy
                    }
                    for (k in v.keys()) walk(v.get(k), path + k)
                }
                is JSONArray -> for (i in 0 until v.length()) walk(v.get(i), path + i.toString())
            }
        }
        walk(root, emptyList())
        return out
    }

    @Test
    fun everyFrameRefusesAnUnknownMemberInEveryObject() {
        var tried = 0
        for (f in Fixtures.frames("server")) {
            for ((path, frame) in withExtraMember(f.json)) {
                tried++
                val e = assertThrows("server/${f.name} accepts an unknown member at '$path'", FrameException::class.java) {
                    ServerFrames.decode(StrictJson.compact(frame))
                }
                // The message says which rule failed.
                assertNotNull(e.message)
            }
        }
        for (f in Fixtures.frames("client")) {
            for ((path, frame) in withExtraMember(f.json)) {
                tried++
                val e = assertThrows("client/${f.name} accepts an unknown member at '$path'", FrameException::class.java) {
                    ClientFrames.decode(StrictJson.compact(frame))
                }
                assertNotNull(e.message)
            }
        }
        assertTrue("$tried cases", tried > 60)
    }

    @Test
    fun aDuplicateMemberIsRefusedInAFrameAndInOp() {
        refused { ClientFrames.decode("""{"t":"ack","id":"${"0".repeat(32)}","stage":"received","stage":"received"}""") }
        val approval = Fixtures.golden("server", "request-approval.json").text
        val dup = approval.replace("\"host\": \"prod\",", "\"host\": \"prod\", \"host\": \"prod\",")
        assertTrue(dup != approval)
        refused { ServerFrames.decode(dup) }
    }

    @Test
    fun aMemberWithTheWrongTypeIsRefusedNotConverted() {
        val base = Fixtures.golden("server", "settled-answered.json").json
        fun with(key: String, value: Any): String = StrictJson.compact(StrictJson.parseObject(base.toString()).put(key, value))
        // `at_ms` as a text, `reason` as a number, `state` as an object: a library would convert them.
        refused { ServerFrames.decode(with("at_ms", "1788000300000")) }
        refused { ServerFrames.decode(with("reason", 5)) }
        refused { ServerFrames.decode(with("state", JSONObject())) }
        refused { ServerFrames.decode(with("at_ms", 1.5)) }
        // A null member counts as absent: a null `reason` is a missing member.
        refused { ServerFrames.decode(with("reason", JSONObject.NULL)) }
        // A null optional member is accepted.
        ServerFrames.decode(with("decision", JSONObject.NULL))
    }

    // ------------------------------------------------------------ syntax

    @Test
    fun theReaderIsStrict() {
        val id = "0".repeat(32)
        val ack = """{"t":"ack","id":"$id","stage":"received"}"""
        assertNotNull(ClientFrames.decode(ack))
        refused { ClientFrames.decode("$ack x") }
        refused { ClientFrames.decode("$ack$ack") }
        refused { ClientFrames.decode("") }
        refused { ClientFrames.decode("[]") }
        refused { ClientFrames.decode("""{"t":"ping","nonce":01}""") }
        refused { ClientFrames.decode("""{"t":"ping","nonce":+1}""") }
        refused { ClientFrames.decode("""{"t":"ping","nonce":-1}""") }
        refused { ClientFrames.decode("""{"t":"ping","nonce":1.0}""") }
        refused { ClientFrames.decode("""{"t":"ping","nonce":1e2}""") }
        refused { ClientFrames.decode("""{"t":"ping","nonce":18446744073709551616}""") }
        refused { ClientFrames.decode("""{"t":"ping","nonce":1,}""") }
        refused { ClientFrames.decode("""{'t':'ping','nonce':1}""") }
        refused { ClientFrames.decode("{\"t\":\"ping\",\"nonce\":1}\u0000") }
        refused { ClientFrames.decode("""{"t":"ref\u0000resh"}""") }
        refused { ClientFrames.decode("{\"t\":\"re\nfresh\"}") }
        refused { ClientFrames.decode("""{"t":"\ud800"}""") }
        refused { ClientFrames.decode("""{"t":"\udc00x"}""") }
        refused { ClientFrames.decode("""{"t":"ready"} {"t":"ready"}""") }
        refused { ClientFrames.decode("""{"t":"ready","x":null}""") }
        // A pair of surrogates is one character.
        assertEquals("😀", StrictJson.parse("\"\\ud83d\\ude00\""))
        // The frame must be UTF-8.
        refused { ClientFrames.decode(byteArrayOf('{'.code.toByte(), 0xC3.toByte(), '}'.code.toByte())) }
        refused { ServerFrames.decode(byteArrayOf(0xFF.toByte())) }
        // Too deep.
        refused { StrictJson.parse("[".repeat(200) + "]".repeat(200)) }
    }

    @Test
    fun opIsRestrictedToTheIntegersOfTheEffectRule() {
        val base = Fixtures.golden("server", "request-approval.json").text
        fun withOp(x: String): String = base.replace("\"ch_user\": \"chdash_mcp_write\"", "\"ch_user\": $x")
        assertTrue(withOp("7") != base)
        for (ok in listOf("7", "-7", "0", "9007199254740991", "-9007199254740991", "null", "true", "[1,{\"a\":null}]", "{}")) {
            assertNotNull(ok, ServerFrames.decode(withOp(ok)))
        }
        for (bad in listOf("1.5", "1.0", "1e2", "-0", "9007199254740992", "-9007199254740992", "18446744073709551616", "0.5")) {
            refused { ServerFrames.decode(withOp(bad)) }
        }
        // Nested too: a float in an array of op.
        refused { ServerFrames.decode(withOp("[[[0.5]]]")) }
        // `op` that is not an object.
        refused { ServerFrames.decode(base.replace(Regex("\"op\": \\{[^}]*\\}"), "\"op\": [1, 2]")) }
    }

    // ------------------------------------------------------------ limits

    private fun question(): JSONObject = Fixtures.golden("server", "request-question-choice.json").json

    private fun decodes(frame: JSONObject): Boolean = try {
        ServerFrames.decode(StrictJson.compact(frame))
        true
    } catch (e: FrameException) {
        false
    }

    @Test
    fun theTitleOfAnItemIsOneSafeLine() {
        for (bad in listOf("a\nb", "a\rb", "a\u0007b", "a\u0000b", "a\u007fb", "a\u0085b", "a b", "\t", "", "a b")) {
            val q = question().put("title", bad)
            assertFalse("question title ${bad.map { it.code }}", decodes(q))
        }
        for (bidi in listOf('‪', '‮', '⁦', '⁩')) {
            assertFalse("bidi ${bidi.code}", decodes(question().put("title", "a${bidi}b")))
        }
        // 128 characters pass and 129 do not. A character counts once, whatever its size in bytes.
        assertTrue(decodes(question().put("title", "é".repeat(128))))
        assertFalse(decodes(question().put("title", "é".repeat(129))))
        // A character outside the BMP counts once too.
        assertTrue(decodes(question().put("title", "😀".repeat(128))))
        assertFalse(decodes(question().put("title", "😀".repeat(129))))
    }

    @Test
    fun theTextLimitsAreExact() {
        fun ok(f: (JSONObject) -> Unit): Boolean = decodes(question().also(f))
        assertTrue(ok { it.put("explanation", "x".repeat(8000)) })
        assertFalse(ok { it.put("explanation", "x".repeat(8001)) })
        assertFalse(ok { it.put("explanation", "") })
        assertTrue(ok { it.getJSONObject("command").put("text", "x".repeat(32768)) })
        assertFalse(ok { it.getJSONObject("command").put("text", "x".repeat(32769)) })
        // The command limit counts bytes: 16385 characters of two bytes are too many.
        assertFalse(ok { it.getJSONObject("command").put("text", "é".repeat(16385)) })
        assertTrue(ok { it.getJSONObject("command").put("text", "é".repeat(16384)) })
        assertTrue(ok { it.getJSONObject("command").put("label", "x".repeat(32)) })
        assertFalse(ok { it.getJSONObject("command").put("label", "x".repeat(33)) })
        assertTrue(ok { it.getJSONObject("input").getJSONArray("options").getJSONObject(0).put("description", "x".repeat(160)) })
        assertFalse(ok { it.getJSONObject("input").getJSONArray("options").getJSONObject(0).put("description", "x".repeat(161)) })
        assertFalse(ok { it.getJSONObject("input").getJSONArray("options").getJSONObject(0).put("label", "x".repeat(49)) })
        assertFalse(ok { it.getJSONObject("input").getJSONArray("options").getJSONObject(0).put("id", "A") })
        assertFalse(ok { it.getJSONObject("from").put("label", "x".repeat(65)) })
        assertFalse(ok { it.getJSONObject("from").put("via", "web") })
        assertFalse(ok { it.put("urgency", "urgent") })
        assertFalse(ok { it.put("created_at_ms", -1) })
        // The command and the explanation of a question keep their control characters: the screen shows them.
        assertTrue(ok { it.getJSONObject("command").put("text", "a‮b\u0001c") })
    }

    // ------------------------------------------------------------ client frames

    @Test
    fun aClientFrameIsCheckedBeforeItIsWritten() {
        val id = "a".repeat(32)
        refused { ClientFrames.encode(AnswerFrame(id, "", Action.ACCEPT)) }
        refused { ClientFrames.encode(AnswerFrame(id, "m 1", Action.ACCEPT)) }
        refused { ClientFrames.encode(AnswerFrame("A".repeat(32), "m-1", Action.ACCEPT)) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.DECLINE, shownEffect = "sha256:" + "0".repeat(64))) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, shownEffect = "sha256:xyz")) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, intent = Intent.COMMENT)) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.DECLINE, intent = Intent.COMMENT)) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.DECLINE, text = "a\u0000b")) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.DECLINE, text = "x".repeat(8001))) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, choice = List(9) { "a$it" })) }
        refused { ClientFrames.encode(ReplyFrame(id, "m-1", "")) }
        refused { ClientFrames.encode(SetPrefsFrame(Prefs(QuietPrefs("22:00", "22:00", "UTC", true), null))) }
        refused { ClientFrames.encode(SetPrefsFrame(Prefs(null, DigestPrefs(DigestMode.EVERY, null, null, 0)))) }
        refused { ClientFrames.encode(SetPrefsFrame(Prefs(null, DigestPrefs(DigestMode.OFF, "09:00", null, null)))) }
        assertNotNull(ClientFrames.encode(AnswerFrame(id, "m-1", Action.DECLINE, text = "x".repeat(8000), intent = Intent.COMMENT)))
    }

    @Test
    fun aFrameThatDoesNotFitOneNoiseMessageIsRefused() {
        val id = "a".repeat(32)
        // 500 entries fit, 501 do not exist.
        val sync = SyncFrame(List(MAX_SYNC_ENTRIES) { OpenRef(id, (Long.MAX_VALUE shr 11)) })
        assertTrue(ClientFrames.encode(sync).size <= MAX_PLAINTEXT)
        refused { ClientFrames.encode(SyncFrame(List(MAX_SYNC_ENTRIES + 1) { OpenRef(id, 0) })) }
        // An answer with a text of the maximum length fits, with two-byte characters.
        val answer = AnswerFrame(id, "m".repeat(64), Action.DECLINE, text = "é".repeat(MAX_USER_TEXT_CHARS), intent = Intent.COMMENT)
        assertTrue(ClientFrames.encode(answer).size <= MAX_PLAINTEXT)
        // A control character costs six bytes in JSON: 8000 of them are 48000 bytes and still fit,
        // but the same text plus the rest of the frame is checked, never cut.
        val control = AnswerFrame(id, "m-1", Action.DECLINE, text = "\u0001".repeat(MAX_USER_TEXT_CHARS))
        assertTrue(ClientFrames.encode(control).size <= MAX_PLAINTEXT)
    }

    @Test
    fun aServerFrameKeepsItsDeliveryIdToAcknowledge() {
        // The receipt names `id` for the frames of the queue, and `alert_id` for an alert.
        assertEquals("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", (Fixtures.server("request-approval.json") as RequestFrame).id)
        assertEquals("11111111111111111111111111111111", (Fixtures.server("message-agent.json") as MessageFrame).id)
        assertEquals("33333333333333333333333333333333", (Fixtures.server("settled-answered.json") as SettledFrame).id)
        assertEquals("66666666666666666666666666666666", (Fixtures.server("digest.json") as DigestFrame).id)
        assertEquals("cccccccccccccccccccccccccccccccc", (Fixtures.server("alert.json") as ServerAlertFrame).alertId)
    }

    // ------------------------------------------------------------ markdown, language, forms

    @Test
    fun theMarkdownAndLanguageMembersReadToTheTypedValues() {
        val q = (Fixtures.server("request-question-markdown.json") as RequestFrame).question!!
        assertEquals(ExplanationFormat.MARKDOWN, q.explanationFormat)
        assertEquals(Language.SQL, q.command!!.language)
        assertTrue(q.explanation!!.contains("```sql"))
        val a = (Fixtures.server("request-approval-markdown.json") as RequestFrame).approval!!
        assertEquals(ExplanationFormat.MARKDOWN, a.explanationFormat)
        assertEquals(Language.SQL, a.language)
        val m = Fixtures.server("message-agent-markdown.json") as MessageFrame
        assertEquals(ExplanationFormat.MARKDOWN, m.explanationFormat)
        assertEquals(Language.BASH, m.command!!.language)
        // Without the members, the format is plain.
        assertNull((Fixtures.server("request-question-choice.json") as RequestFrame).question!!.explanationFormat)
        // `plain` is a valid spelling. The frame writes back what it read.
        val plain = Fixtures.golden("server", "request-question-markdown.json").json.put("explanation_format", "plain")
        val frame = ServerFrames.decode(StrictJson.compact(plain)) as RequestFrame
        assertEquals(ExplanationFormat.PLAIN, frame.question!!.explanationFormat)
        same(plain, frame.toJson(), "plain format")
    }

    @Test
    fun everyLanguageOfTheClosedListIsAValidHint() {
        assertEquals(
            listOf("plain", "sql", "json", "yaml", "toml", "bash", "python", "rust", "javascript", "typescript", "diff"),
            Language.entries.map { it.wire },
        )
        val base = Fixtures.golden("server", "request-question-markdown.json").json
        for (l in Language.entries) {
            base.getJSONObject("command").put("language", l.wire)
            ServerFrames.decode(StrictJson.compact(base))
        }
        for (bad in listOf("cobol", "SQL", "Rust", "", "sh", "html")) {
            base.getJSONObject("command").put("language", bad)
            refused { ServerFrames.decode(StrictJson.compact(base)) }
        }
    }

    @Test
    fun theGoldenFormReadsToTheTypedValues() {
        val r = Fixtures.server("request-question-form.json") as RequestFrame
        val input = r.question!!.input
        assertEquals(InputType.FORM, input.type)
        val questions = input.questions!!
        assertEquals(listOf("target", "checks", "window", "note"), questions.map { it.id })
        assertEquals(FormQuestionType.CHOICE, questions[0].type)
        assertEquals(FormQuestionType.TEXT, questions[3].type)
        assertEquals(3, questions[1].max)
        assertTrue(questions[1].multiple)
        assertEquals("Which checks do you run?\nTick all that apply.", questions[1].text)
        assertEquals("Shared test cluster", questions[0].options[0].description)
        assertEquals("No secret here", questions[3].placeholder)
        assertTrue(r.allowCustom && r.allowAskBack)
        // The other kinds of input have no questions.
        assertNull((Fixtures.server("request-question-choice.json") as RequestFrame).question!!.input.questions)
    }

    @Test
    fun theGoldenAnswerOfAFormReadsAndWritesBack() {
        val a = ClientFrames.decode(Fixtures.golden("client", "answer-form.json").text) as AnswerFrame
        assertEquals(
            listOf(
                FormAnswer("target", listOf("staging")),
                FormAnswer("checks", listOf("unit", "e2e")),
                FormAnswer("window", text = "Friday morning"),
                FormAnswer("note", text = "Use the blue pipeline."),
            ),
            a.answers,
        )
        assertNull(a.choice)
        assertNull(a.text)
        assertEquals(a, ClientFrames.decode(ClientFrames.encode(a)))
    }

    @Test
    fun theFormRulesAreRefusedWithTheNamedReason() {
        // The same table as `new_invalid_frames_fail_for_the_named_reason` of `inbox-core/tests/frames.rs`.
        val server = mapOf(
            "form-zero-questions" to "input.questions",
            "form-five-questions" to "input.questions",
            "form-duplicate-ids" to "form.questions.id",
            "form-one-option" to "form.questions.options",
            "form-nine-options" to "form.questions.options",
            "form-min-above-max" to "form.questions.options",
            "form-single-with-max-two" to "form.questions.options",
            "form-text-with-options" to "form.questions.options",
            "form-header-17-chars" to "form.questions.header",
            "form-header-newline" to "form.questions.header",
            "form-text-601-chars" to "form.questions.text",
            "form-question-id-uppercase" to "form.questions.id",
            "form-unknown-question-type" to "type",
            "form-with-top-level-options" to "input.options",
            "form-without-questions" to "input.questions",
            "questions-on-a-choice-input" to "input.questions",
            "form-on-one-shot-key" to "request.form",
            "form-allow-custom-with-text-only" to "request.allow_custom",
            "explanation-format-html" to "explanation_format",
            "explanation-format-uppercase" to "explanation_format",
            "explanation-format-without-explanation" to "request.explanation_format",
            "command-language-unknown" to "language",
            "command-language-uppercase" to "language",
            "approval-explanation-format-without-explanation" to "approval.explanation_format",
            "approval-language-unknown" to "language",
            "approval-with-top-level-explanation-format" to "request.approval",
            "message-explanation-format-without-explanation" to "message.explanation_format",
            "message-user-with-explanation-format" to "message.explanation",
        )
        for ((name, reason) in server) {
            val e = assertThrows("$name was accepted", FrameException::class.java) {
                ServerFrames.decode(Fixtures.golden("invalid/server", "$name.json").text)
            }
            assertTrue("$name: '${e.message}' does not name '$reason'", e.message!!.contains(reason))
        }
        val client = mapOf(
            "answer-form-with-choice" to "answer.answers",
            "answer-form-with-text" to "answer.answers",
            "answer-form-with-decline" to "answer.answers",
            "answer-form-with-intent" to "answer.intent",
            "answer-form-empty-list" to "answer.answers",
            "answer-form-five-entries" to "answer.answers",
            "answer-form-duplicate-question-id" to "answer.answers.question_id",
            "answer-form-entry-with-both" to "answer.answers",
            "answer-form-entry-with-neither" to "answer.answers",
            "answer-form-entry-empty-choice" to "answer.answers.choice",
            "answer-form-entry-nine-choices" to "answer.answers.choice",
            "answer-form-entry-repeated-choice" to "answer.answers.choice",
            "answer-form-entry-text-8001-chars" to "answer.answers.text",
            "answer-form-entry-bad-question-id" to "answer.answers.question_id",
            "answer-form-entry-unknown-member" to "unknown member",
        )
        for ((name, reason) in client) {
            val e = assertThrows("$name was accepted", FrameException::class.java) {
                ClientFrames.decode(Fixtures.golden("invalid/client", "$name.json").text)
            }
            assertTrue("$name: '${e.message}' does not name '$reason'", e.message!!.contains(reason))
        }
    }

    @Test
    fun theWriterRefusesWhatTheReaderRefusesForAFormAnswer() {
        val id = "a".repeat(32)
        val ticks = FormAnswer("one", listOf("a"))
        assertNotNull(ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, answers = listOf(ticks, FormAnswer("two", text = "x")))))
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.DECLINE, answers = listOf(ticks))) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, answers = emptyList())) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, choice = listOf("a"), answers = listOf(ticks))) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, text = "x", answers = listOf(ticks))) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, answers = listOf(ticks, ticks))) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, answers = List(5) { FormAnswer("q$it", text = "x") })) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, answers = listOf(FormAnswer("one", listOf("a"), "x")))) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, answers = listOf(FormAnswer("one")))) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, answers = listOf(FormAnswer("one", listOf("a", "a"))))) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, answers = listOf(FormAnswer("One", listOf("a"))))) }
        refused { ClientFrames.encode(AnswerFrame(id, "m-1", Action.ACCEPT, answers = listOf(FormAnswer("one", text = "x".repeat(8001))))) }
    }

    @Test
    fun theLargestFormFitsOneNoiseMessage() {
        val json = Fixtures.golden("server", "request-question-form.json").json
        val question = json.getJSONObject("input").getJSONArray("questions")
        val big = JSONArray()
        for (n in 0 until 4) {
            val q = JSONObject().put("id", "q$n").put("header", "h".repeat(16)).put("text", "t".repeat(600))
                .put("type", "choice").put("multiple", true).put("min", 1).put("max", 8)
            val options = JSONArray()
            for (i in 0 until 8) {
                options.put(JSONObject().put("id", "o$i").put("label", "L".repeat(48)).put("description", "D".repeat(160)))
            }
            big.put(q.put("options", options))
        }
        assertEquals(4, question.length())
        json.getJSONObject("input").put("questions", big)
        json.put("explanation", "e".repeat(MAX_EXPLANATION_CHARS)).put("explanation_format", "markdown")
        json.put("command", JSONObject().put("text", "c".repeat(MAX_COMMAND_BYTES)).put("language", "sql"))
        val bytes = StrictJson.compact(json).toByteArray()
        assertTrue("${bytes.size} bytes", bytes.size <= MAX_PLAINTEXT)
        ServerFrames.decode(bytes)
    }

    // ------------------------------------------------------------ agents

    private fun server(name: String): JSONObject = Fixtures.golden("server", name).json

    private fun client(name: String): JSONObject = Fixtures.golden("client", name).json

    private fun String.hex(): String = repeat(16)

    @Test
    fun theRequestFromHasTheAgentMembersAndRefusesUnknown() {
        val form = Fixtures.server("request-question-form.json") as RequestFrame
        assertEquals(
            Sender("claude-laptop #4", "claude-laptop", "alice", Via.MCP, Interaction.CONVERSATIONAL, "d4".hex(), AgentKind.AGENT, AgentState.NEW),
            form.from,
        )
        assertEquals("billing-bot", form.agentLabel)
        val bucket = Fixtures.server("request-question-confirm-one-shot.json") as RequestFrame
        assertEquals(AgentKind.BUCKET, bucket.from.agentKind)
        assertEquals(AgentState.KNOWN, bucket.from.agentState)
        assertNull(bucket.agentLabel)
        val base = server("request-approval.json")
        // Each new member of `from` is mandatory.
        for (key in listOf("agent_id", "agent_kind", "agent_state")) {
            val v = server("request-approval.json")
            v.getJSONObject("from").remove(key)
            assertFalse("from without $key", decodes(v))
        }
        // `agent_label` is optional, and a null is an absent member.
        assertTrue(decodes(JSONObject(base.toString()).also { it.remove("agent_label") }))
        assertTrue(decodes(JSONObject(base.toString()).put("agent_label", JSONObject.NULL)))
        // An open request has no `blocked` or `merged` agent, and a bucket is `known`.
        for (state in listOf("blocked", "merged")) {
            assertFalse(state, decodes(server("request-approval.json").also { it.getJSONObject("from").put("agent_state", state) }))
        }
        val asBucket = server("request-approval.json").also { it.getJSONObject("from").put("agent_kind", "bucket") }
        assertTrue(decodes(asBucket))
        assertFalse(decodes(asBucket.also { it.getJSONObject("from").put("agent_state", "new") }))
        assertFalse(decodes(server("request-approval.json").also { it.getJSONObject("from").put("agent_id", "A1".hex()) }))
    }

    @Test
    fun theTextThatAnAgentSaysIsOneLineWithoutHiddenCharacters() {
        fun ok(text: String): Boolean = decodes(server("request-approval.json").put("agent_label", text))
        assertTrue(ok("x"))
        assertTrue(ok("x".repeat(64)))
        assertTrue(ok("é".repeat(64)))
        assertTrue("a character outside the BMP counts once", ok("😀".repeat(64)))
        assertFalse(ok(""))
        assertFalse(ok("x".repeat(65)))
        assertFalse(ok("😀".repeat(65)))
        for (bad in listOf("a\nb", "a\rb", "a\tb", "a\u0007b", "a\u0000b", "a\u0085b", "a\u202eb", "a\u2066b", "a\u200bb", "a\u200db", "a\ufeffb", "a\u00adb", "a\u2028b", "a\u2029b", "a\udb40\udc41b")) {
            assertFalse("agent_label ${bad.map { it.code }}", ok(bad))
        }
        assertTrue(ok("billing bot"))
        assertTrue(ok("billing-bot "))
        assertFalse(decodes(server("request-approval.json").put("agent_label", 7)))
        // The check of a character of the class Cf is the list of the core crate.
        for (cp in listOf(0xAD, 0x200B, 0x200D, 0x200E, 0x202E, 0x2060, 0x2066, 0xFEFF, 0xE0001)) assertTrue(cp.toString(16), Check.isFormatChar(cp))
        for (cp in listOf('a'.code, ' '.code, 0xE9, '-'.code, 0xB7, 0x2028, '\n'.code, 0x4E2D)) assertFalse(cp.toString(16), Check.isFormatChar(cp))
    }

    @Test
    fun theAgentFramesReadToTheTypedValues() {
        val a = Fixtures.server("agent-new.json") as AgentFrame
        assertEquals(Triple(3L, AgentKind.AGENT, AgentState.NEW), Triple(a.rev, a.kind, a.state))
        assertTrue(a.lockAccept && !a.named)
        assertEquals("claude-laptop #4", a.label)
        assertEquals("claude-laptop", a.keyName)
        assertEquals(1L, a.open)
        assertEquals(Observed("billing-bot", 1, "claude-code/2.1.0", "claude-code/2.1.0 (linux; x64)", "203.0.113.0/24", 2), a.observed)
        assertEquals(Similar("a1".hex(), "Billing bot"), a.similar)
        val known = Fixtures.server("agent-known.json") as AgentFrame
        assertEquals(Observed(null, null, "claude-code/2.1.0", null, null, 1), known.observed)
        assertTrue(known.named && !known.lockAccept)
        assertNull(known.similar)
        assertEquals(AgentState.BLOCKED, (Fixtures.server("agent-blocked.json") as AgentFrame).state)
        val merged = Fixtures.server("agent-merged.json") as AgentFrame
        assertEquals(AgentState.MERGED, merged.state)
        assertEquals("a1".hex(), merged.mergedInto)
        val bucket = Fixtures.server("agent-bucket.json") as AgentFrame
        assertEquals(Triple(AgentKind.BUCKET, AgentState.KNOWN, null), Triple(bucket.kind, bucket.state, bucket.observed))
        assertEquals("Unattributed · claude-laptop", bucket.label)
        val r = Fixtures.server("agent-result.json") as AgentResultFrame
        assertEquals(AgentResultStatus.OK, r.status)
        assertEquals("m-0010", r.clientMsgId)
        assertEquals(AgentResultStatus.NAME_TAKEN, (Fixtures.server("agent-result-name-taken.json") as AgentResultFrame).status)
        val locked = Fixtures.server("answer-result-agent-locked.json") as AnswerResultFrame
        assertEquals(AnswerStatus.AGENT_LOCKED, locked.status)
        assertEquals(ItemState.PENDING, locked.state)
    }

    @Test
    fun theAgentFrameRulesAndLimitsAreExact() {
        val base = "agent-new.json"
        fun ok(f: (JSONObject) -> Unit): Boolean = decodes(server(base).also(f))
        // The mandatory members, and the two optional ones.
        for (key in listOf("id", "agent_id", "rev", "key_name", "kind", "state", "label", "named", "created_at_ms", "last_seen_ms", "open", "lock_accept")) {
            assertFalse("without $key", ok { it.remove(key) })
        }
        assertTrue(ok { it.remove("observed") })
        assertTrue(ok { it.remove("similar") })
        assertTrue(ok { it.put("observed", JSONObject().put("nets_seen", 0)) })
        assertFalse(ok { it.put("observed", JSONObject()) })
        // The texts count characters, not bytes, and refuse a hidden character.
        for ((name, max) in listOf("says" to 64, "client" to 64, "user_agent" to 128, "net" to 43)) {
            assertTrue("$name at the limit", ok { it.getJSONObject("observed").put(name, "é".repeat(max)) })
            assertFalse("$name above the limit", ok { it.getJSONObject("observed").put(name, "é".repeat(max + 1)) })
            assertFalse("$name empty", ok { it.getJSONObject("observed").put(name, "") })
            assertFalse("$name bidi", ok { it.getJSONObject("observed").put(name, "a\u202eb") })
            assertFalse("$name zero width", ok { it.getJSONObject("observed").put(name, "a\u200bb") })
            assertFalse("$name line end", ok { it.getJSONObject("observed").put(name, "a\nb") })
            assertFalse("$name as a number", ok { it.getJSONObject("observed").put(name, 7) })
        }
        for (name in listOf("label", "key_name")) {
            assertTrue(ok { it.put(name, "é".repeat(64)) })
            assertFalse(ok { it.put(name, "é".repeat(65)) })
            assertFalse(ok { it.put(name, "") })
        }
        assertFalse(ok { it.getJSONObject("similar").put("label", "é".repeat(65)) })
        // `nets_seen` is 0 to 8. `says_changed` is 1 or more, and needs `says`.
        for (n in 0..8) assertTrue("$n", ok { it.getJSONObject("observed").put("nets_seen", n) })
        for (bad in listOf<Any>(9, -1, 256, 1.5, "2")) assertFalse("$bad", ok { it.getJSONObject("observed").put("nets_seen", bad) })
        assertTrue(ok { it.getJSONObject("observed").put("says_changed", 4294967295L) })
        assertFalse(ok { it.getJSONObject("observed").put("says_changed", 0) })
        assertFalse(ok { it.getJSONObject("observed").put("says_changed", 4294967296L) })
        assertFalse(ok { it.getJSONObject("observed").remove("says") })
        // `rev` is 1 to the largest signed 64-bit integer.
        assertTrue(ok { it.put("rev", 1) })
        assertTrue(ok { it.put("rev", Long.MAX_VALUE) })
        assertFalse(ok { it.put("rev", 0) })
        assertFalse(ok { it.put("rev", -1) })
        assertFalse(decodes(JSONObject(StrictJson.compact(server(base)).replace(Regex("\"rev\":3"), "\"rev\":9223372036854775808"))))
        assertFalse(ok { it.put("rev", 1.5) })
        // `open` is a u32. The times are 0 or more.
        assertTrue(ok { it.put("open", 4294967295L) })
        assertFalse(ok { it.put("open", 4294967296L) })
        assertFalse(ok { it.put("open", -1) })
        assertTrue(ok { it.put("last_seen_ms", 0) })
        assertFalse(ok { it.put("last_seen_ms", -1) })
        assertFalse(ok { it.put("created_at_ms", -1) })
        // The states. `merged` needs its target and only `merged` has one. `lock_accept` and `similar` go with `new` only.
        val known = "agent-known.json"
        fun okKnown(f: (JSONObject) -> Unit): Boolean = decodes(server(known).also(f))
        assertTrue(okKnown { it.put("state", "blocked") })
        assertFalse(okKnown { it.put("state", "merged") })
        assertTrue(okKnown { it.put("state", "merged").put("merged_into", "c".repeat(32)) })
        assertFalse(okKnown { it.put("state", "merged").put("merged_into", it.getString("agent_id")) })
        assertFalse(okKnown { it.put("merged_into", "c".repeat(32)) })
        assertFalse(okKnown { it.put("lock_accept", true) })
        assertFalse(okKnown { it.put("similar", JSONObject().put("agent_id", "d4".hex()).put("label", "x")) })
        assertFalse(ok { it.getJSONObject("similar").put("agent_id", it.getString("agent_id")) })
        assertTrue(ok { it.put("lock_accept", false) })
        // The bucket is `known`, has no name, no hints and no lock.
        val bucket = "agent-bucket.json"
        fun okBucket(f: (JSONObject) -> Unit): Boolean = decodes(server(bucket).also(f))
        assertTrue(okBucket { })
        assertFalse(okBucket { it.put("state", "new") })
        assertFalse(okBucket { it.put("state", "blocked") })
        assertFalse(okBucket { it.put("named", true) })
        assertFalse(okBucket { it.put("observed", JSONObject().put("nets_seen", 0)) })
        assertFalse(okBucket { it.put("merged_into", "c".repeat(32)) })
        // The value of each closed list.
        for (state in listOf("new", "known", "blocked")) assertTrue(state, okKnown { it.put("state", state).remove("lock_accept").also { _ -> it.put("lock_accept", false) } })
        for (bad in listOf("gone", "New", "")) assertFalse(bad, ok { it.put("state", bad) })
        for (bad in listOf("robot", "Agent", "")) assertFalse(bad, ok { it.put("kind", bad) })
    }

    @Test
    fun theAgentResultStatusIsClosedAndTheFrameHasNoDeliveryId() {
        val base = "agent-result.json"
        for (status in AgentResultStatus.entries) {
            val f = ServerFrames.decode(StrictJson.compact(server(base).put("status", status.wire))) as AgentResultFrame
            assertEquals(status, f.status)
        }
        assertEquals(
            listOf("ok", "not_found", "invalid_name", "name_taken", "bad_state", "cross_key", "not_enabled"),
            AgentResultStatus.entries.map { it.wire },
        )
        for (bad in listOf("", "OK", "Ok", "done", "agent_locked", "accepted", "not-found", "unknown")) {
            assertFalse(bad, decodes(server(base).put("status", bad)))
        }
        assertFalse(decodes(server(base).put("state", "known")))
        assertFalse(decodes(server(base).put("id", "a".repeat(32))))
        assertFalse(decodes(server(base).put("agent_id", "abc")))
        assertFalse(decodes(server(base).put("client_msg_id", "")))
        assertFalse(decodes(server(base).also { it.remove("agent_id") }))
        // `agent_locked` is a status of an answer, and not of an agent result.
        assertTrue(AnswerStatus.entries.any { it.wire == "agent_locked" })
    }

    @Test
    fun agentDoHasFiveOpsAndEachTakesItsMembers() {
        val a = "d4".hex()
        val to = "a1".hex()
        fun ok(op: String, vararg extra: Pair<String, Any>): Boolean = try {
            val o = JSONObject().put("t", "agent_do").put("agent_id", a).put("client_msg_id", "m-0010").put("op", op)
            for ((k, v) in extra) o.put(k, v)
            ClientFrames.decode(StrictJson.compact(o))
            true
        } catch (e: FrameException) {
            false
        }
        assertTrue(ok("rename", "name" to "Refund cleaner"))
        assertTrue(ok("allow"))
        assertTrue(ok("allow", "name" to "Refund cleaner"))
        assertTrue(ok("block"))
        assertTrue(ok("unblock"))
        assertTrue(ok("merge", "into" to to))
        assertTrue("a merge into the agent itself is well formed: the server answers bad_state", ok("merge", "into" to a))
        assertFalse(ok("rename"))
        assertFalse(ok("rename", "name" to "x", "into" to to))
        assertFalse(ok("allow", "into" to to))
        assertFalse(ok("block", "name" to "x"))
        assertFalse(ok("block", "into" to to))
        assertFalse(ok("unblock", "name" to "x"))
        assertFalse(ok("merge"))
        assertFalse(ok("merge", "name" to "x", "into" to to))
        assertFalse(ok("merge", "into" to "A1".hex()))
        assertFalse(ok("merge", "into" to "a1"))
        for (bad in listOf("delete", "Rename", "")) assertFalse(bad, ok(bad))
        // The name has 1 to 32 characters, with no NUL. The other rules of a name are the rules of the server.
        for (op in listOf("rename", "allow")) {
            assertTrue(ok(op, "name" to "n"))
            assertTrue(ok(op, "name" to "n".repeat(32)))
            assertTrue("characters, not bytes", ok(op, "name" to "é".repeat(32)))
            assertTrue(ok(op, "name" to "😀".repeat(32)))
            assertFalse(ok(op, "name" to "n".repeat(33)))
            assertFalse(ok(op, "name" to "é".repeat(33)))
            assertFalse(ok(op, "name" to ""))
            assertFalse(ok(op, "name" to "a\u0000b"))
            assertFalse(ok(op, "name" to 7))
            assertTrue(ok(op, "name" to "a\u202eb"))
            assertTrue(ok(op, "name" to " padded "))
        }
        assertTrue(ok("allow", "name" to JSONObject.NULL, "into" to JSONObject.NULL))
        // `client_msg_id` follows the rule of `answer`.
        for ((id, valid) in listOf("m" to true, "M-1_z" to true, "m".repeat(64) to true, "" to false, "m".repeat(65) to false, "a b" to false, "a/b" to false)) {
            val o = client("agent-do-block.json").put("client_msg_id", id)
            assertEquals(id, valid, try { ClientFrames.decode(StrictJson.compact(o)); true } catch (e: FrameException) { false })
        }
        // The typed value, and the writer refuses what the reader refuses.
        val f = ClientFrames.decode(Fixtures.golden("client", "agent-do-merge.json").text)
        assertEquals(AgentDoFrame(a, "m-0010", AgentOp.MERGE, null, to), f)
        assertEquals(f, ClientFrames.decode(ClientFrames.encode(f)))
        refused { ClientFrames.encode(AgentDoFrame(a, "m-1", AgentOp.RENAME)) }
        refused { ClientFrames.encode(AgentDoFrame(a, "m-1", AgentOp.BLOCK, name = "x")) }
        refused { ClientFrames.encode(AgentDoFrame(a, "m-1", AgentOp.MERGE)) }
        refused { ClientFrames.encode(AgentDoFrame(a, "", AgentOp.BLOCK)) }
        refused { ClientFrames.encode(AgentDoFrame("abc", "m-1", AgentOp.BLOCK)) }
        refused { ClientFrames.encode(AgentDoFrame(a, "m-1", AgentOp.RENAME, name = "n".repeat(33))) }
        assertNotNull(ClientFrames.encode(AgentDoFrame(a, "m-1", AgentOp.ALLOW, name = "Refund cleaner")))
        assertFalse(decodesClient(client("agent-do-block.json").put("force", true)))
    }

    private fun decodesClient(frame: JSONObject): Boolean = try {
        ClientFrames.decode(StrictJson.compact(frame))
        true
    } catch (e: FrameException) {
        false
    }

    @Test
    fun syncHasTheAgentsThatTheDeviceHoldsAndTheLimitsAreExact() {
        val item = "bb".hex()
        val a1 = "a1".hex()
        val a2 = "d4".hex()
        fun wire(agents: Any?): JSONObject {
            val o = JSONObject().put("t", "sync").put("open", JSONArray().put(JSONObject().put("request_id", item).put("seq", 2)))
            if (agents != null) o.put("agents", agents)
            return o
        }
        fun entry(id: String, rev: Any) = JSONObject().put("agent_id", id).put("rev", rev)
        // Absent, null and empty: the device holds no agent. An absent list and an empty list read back as they came.
        assertTrue(decodesClient(wire(null)))
        assertTrue(decodesClient(wire(JSONArray())))
        assertTrue(decodesClient(wire(null).put("agents", JSONObject.NULL)))
        assertNull((ClientFrames.decode(StrictJson.compact(wire(null))) as SyncFrame).agents)
        assertEquals(emptyList<AgentRef>(), (ClientFrames.decode(StrictJson.compact(wire(JSONArray()))) as SyncFrame).agents)
        assertTrue(decodesClient(wire(JSONArray().put(entry(a1, 1)).put(entry(a2, Long.MAX_VALUE)))))
        for (bad in listOf<Any>(
            JSONArray().put(entry(a1, 0)), JSONArray().put(entry(a1, -1)), JSONArray().put(JSONObject().put("agent_id", a1)),
            JSONArray().put(JSONObject().put("rev", 1)), JSONArray().put(entry("A1".hex(), 1)), JSONArray().put(entry(a1, 1).put("zz", 1)),
            JSONArray().put(entry(a1, 1)).put(entry(a1, 2)), JSONObject(), "a1",
        )) {
            assertFalse("$bad", decodesClient(wire(bad)))
        }
        assertFalse(decodesClient(wire(JSONArray().put(entry(a1, 1.5)))))
        // At most 200 entries.
        fun list(n: Int) = JSONArray().also { l -> for (i in 1..n) l.put(entry("%032x".format(i), 1)) }
        assertTrue(decodesClient(wire(list(MAX_SYNC_AGENTS))))
        assertFalse(decodesClient(wire(list(MAX_SYNC_AGENTS + 1))))
        // The most items and the most agents together fit in one Noise message, and the writer refuses one more of each.
        val full = SyncFrame(
            List(MAX_SYNC_ENTRIES) { OpenRef(item, Long.MAX_VALUE shr 11) },
            List(MAX_SYNC_AGENTS) { AgentRef("%032x".format(it), Long.MAX_VALUE) },
        )
        assertTrue(ClientFrames.encode(full).size <= MAX_PLAINTEXT)
        refused { ClientFrames.encode(full.copy(agents = full.agents!! + AgentRef("f".repeat(32), 1))) }
        refused { ClientFrames.encode(full.copy(open = full.open + OpenRef(item, 0))) }
        refused { ClientFrames.encode(SyncFrame(emptyList(), listOf(AgentRef(a1, 0)))) }
        refused { ClientFrames.encode(SyncFrame(emptyList(), listOf(AgentRef(a1, 1), AgentRef(a1, 2)))) }
        // The golden frame.
        val g = ClientFrames.decode(Fixtures.golden("client", "sync-agents.json").text) as SyncFrame
        assertEquals(listOf(AgentRef(a1, 5), AgentRef(a2, 1)), g.agents)
        assertEquals(1, g.open.size)
    }

    @Test
    fun theAgentFixturesAreRefusedWithTheNamedRule() {
        // The invalid frames of the agents name the rule that they break, as `agent_invalid_frames_fail_for_the_named_reason`
        // of `inbox-core/tests/frames.rs` does. A few of each family: the full table is on the Rust side.
        val server = mapOf(
            "request-from-agent-state-blocked" to "from.agent_state",
            "request-from-bucket-state-new" to "from.agent_state",
            "request-from-agent-id-uppercase" to "from.agent_id",
            "request-agent-label-bidi" to "request.agent_label",
            "request-agent-label-65-chars" to "request.agent_label",
            "agent-rev-zero" to "agent.rev",
            "agent-merged-without-target" to "agent.merged_into",
            "agent-lock-accept-on-known" to "agent.lock_accept",
            "agent-bucket-named" to "agent.kind",
            "agent-observed-says-bidi" to "agent.observed.says",
            "agent-observed-user-agent-129-chars" to "agent.observed.user_agent",
            "agent-observed-nets-seen-9" to "agent.observed.nets_seen",
            "agent-observed-says-changed-zero" to "agent.observed.says_changed",
            "agent-similar-on-known" to "agent.similar",
            "agent-result-status-unknown" to "status",
            "answer-result-agent-locked-uppercase" to "status",
        )
        for ((name, reason) in server) {
            val e = assertThrows("$name was accepted", FrameException::class.java) {
                ServerFrames.decode(Fixtures.golden("invalid/server", "$name.json").text)
            }
            assertTrue("$name: '${e.message}' does not name '$reason'", e.message!!.contains(reason))
        }
        val client = mapOf(
            "agent-do-rename-without-name" to "agent_do.op",
            "agent-do-block-with-name" to "agent_do.op",
            "agent-do-merge-without-into" to "agent_do.op",
            "agent-do-name-33-chars" to "agent_do.name",
            "agent-do-name-with-nul" to "agent_do.name",
            "agent-do-merge-into-not-hex" to "agent_do.into",
            "agent-do-op-unknown" to "op",
            "sync-agents-rev-zero" to "sync.agents",
            "sync-agents-duplicate-agent" to "sync.agents",
            "sync-agents-201-entries" to "sync.agents",
            "sync-agents-extra-member" to "unknown member",
        )
        for ((name, reason) in client) {
            val e = assertThrows("$name was accepted", FrameException::class.java) {
                ClientFrames.decode(Fixtures.golden("invalid/client", "$name.json").text)
            }
            assertTrue("$name: '${e.message}' does not name '$reason'", e.message!!.contains(reason))
        }
    }

    @Test
    fun theLargestAgentFrameFitsOneNoiseMessage() {
        val observed = JSONObject().put("says", "é".repeat(64)).put("says_changed", 4294967295L).put("client", "é".repeat(64))
            .put("user_agent", "é".repeat(128)).put("net", "n".repeat(43)).put("nets_seen", 8)
        val frame = server("agent-new.json").put("rev", Long.MAX_VALUE).put("key_name", "k".repeat(64)).put("label", "l".repeat(64))
            .put("open", 4294967295L).put("observed", observed)
        frame.getJSONObject("similar").put("label", "s".repeat(64))
        val bytes = StrictJson.compact(frame).toByteArray()
        assertTrue("${bytes.size} bytes", bytes.size <= MAX_PLAINTEXT)
        val read = ServerFrames.decode(bytes) as AgentFrame
        same(frame, read.toJson(), "the largest agent")
    }
}
