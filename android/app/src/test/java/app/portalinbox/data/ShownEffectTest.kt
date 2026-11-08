package app.portalinbox.data

import app.portalinbox.core.EffectResult
import app.portalinbox.core.ResolvedField
import app.portalinbox.core.ResolvedView
import app.portalinbox.core.ViewResult
import app.portalinbox.proto.ApprovalBody
import app.portalinbox.proto.RequestFrame
import app.portalinbox.proto.StrictJson
import app.portalinbox.testsupport.Fixtures
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import app.portalinbox.testsupport.ScriptedEffectHasher
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The effect of a received `op` against the `effect` of the frame. The real hash runs in Rust
 * through JNI and `inbox-core` proves it. These tests use an injected [app.portalinbox.core.EffectHasher].
 */
class ShownEffectTest {
    private fun golden(): ApprovalBody = (Fixtures.server("request-approval.json") as RequestFrame).approval!!

    private fun verdict(a: ApprovalBody, hasher: ReferenceEffectHasher = ReferenceEffectHasher()) = ShownEffect.verify(a, hasher)

    private fun edited(edit: (JSONObject) -> Unit): ApprovalBody {
        val o = Fixtures.golden("server", "request-approval.json").json
        edit(o.getJSONObject("approval"))
        return (app.portalinbox.proto.ServerFrames.decode(StrictJson.compact(o)) as RequestFrame).approval!!
    }

    @Test
    fun theGoldenApprovalHashesToItsEffectAndShowsItsCommand() {
        val v = verdict(golden()) as ShownEffect.Verdict.Valid
        assertEquals("sha256:9e07172b785fdac847ecde1c13a6245449c0a4a691454245dcf155b457014290", v.effect)
        assertEquals("INSERT INTO analytics.events SELECT 1", v.view.body)
        assertEquals("SQL", v.view.bodyLabel)
        assertEquals("ClickHouse write", v.view.title)
        assertEquals(
            listOf(ResolvedField("Host", "prod"), ResolvedField("Database", "analytics"), ResolvedField("Runs as", "chdash_mcp_write")),
            v.view.fields,
        )
        assertEquals("9e07172b", ShownEffect.short(v.effect))
        assertNull(ShownEffect.short("sha256:xyz"))
    }

    @Test
    fun aChangedOpOrAChangedKeyOfTheEffectGivesAMismatch() {
        // The sql changed after the server hashed it.
        val sql = edited { it.getJSONObject("op").put("sql", "DROP TABLE analytics.events") }
        assertEquals(ShownEffect.Verdict.Invalid(Validity.EFFECT_MISMATCH), verdict(sql))
        val added = edited { it.getJSONObject("op").put("extra", 1) }
        assertEquals(ShownEffect.Verdict.Invalid(Validity.EFFECT_MISMATCH), verdict(added))
        for (key in listOf("service", "action", "resource")) {
            val a = edited { it.put(key, it.getString(key) + "x") }
            assertEquals(key, ShownEffect.Verdict.Invalid(Validity.EFFECT_MISMATCH), verdict(a))
        }
        // The effect of the frame changed by one digit.
        val digit = edited { it.put("effect", it.getString("effect").let { e -> e.dropLast(1) + if (e.last() == '0') '1' else '0' }) }
        assertEquals(ShownEffect.Verdict.Invalid(Validity.EFFECT_MISMATCH), verdict(digit))
    }

    @Test
    fun theTextsOutsideTheEffectDoNotChangeIt() {
        val a = edited {
            it.put("explanation", "Another text of the agent.")
            it.getJSONObject("view").put("title", "Another title").put("body_label", "Query")
            it.put("code", "ZZZZ")
        }
        assertTrue(verdict(a) is ShownEffect.Verdict.Valid)
        // The order of the members of op does not matter.
        val reordered = Fixtures.golden("server", "request-approval.json").text.replace(
            "\"tool\": \"run_write_query\",", "",
        ).replace("\"ch_user\": \"chdash_mcp_write\"", "\"ch_user\": \"chdash_mcp_write\", \"tool\": \"run_write_query\"")
        val r = (app.portalinbox.proto.ServerFrames.decode(reordered) as RequestFrame).approval!!
        assertTrue(verdict(r) is ShownEffect.Verdict.Valid)
    }

    @Test
    fun aViewThatNamesNothingOrNamesAnObjectIsInvalid() {
        val missing = edited { it.getJSONObject("view").put("body", "/missing") }
        assertEquals(ShownEffect.Verdict.Invalid(Validity.INVALID_VIEW), verdict(missing))
        val notString = edited { it.getJSONObject("view").put("body", "/tool").put("fields", org.json.JSONArray().put(org.json.JSONArray().put("Op").put(""))) }
        // The empty pointer names the whole `op`: an object is not a field.
        assertEquals(ShownEffect.Verdict.Invalid(Validity.INVALID_VIEW), verdict(notString))
    }

    @Test
    fun theHasherReceivesTheStringsAndTheOpAsReceived() {
        val a = golden()
        val scripted = ScriptedEffectHasher({ _, _, _, _ -> EffectResult.Hash(a.effect) }, { _, _ -> ViewResult.Resolved(ResolvedView("t", emptyList(), "body", null)) })
        val v = ShownEffect.verify(a, scripted) as ShownEffect.Verdict.Valid
        assertEquals("body", v.view.body)
        assertEquals(1, scripted.effectCalls.size)
        val call = scripted.effectCalls[0]
        assertEquals(listOf("datastore", "data.write", "host:prod/database:analytics"), call.take(3))
        // The op that goes to the hash is the op of the frame, nothing else.
        assertTrue(StrictJson.parseObject(call[3]).similar(a.op))
        // The view is read from the same op.
        assertEquals(call[3], scripted.viewCalls.single()[0])
        assertTrue(StrictJson.parseObject(scripted.viewCalls.single()[1]).similar(a.view.toJson()))
    }

    @Test
    fun theHashOfTheHasherDecidesNotTheTextOfTheFrame() {
        val a = golden()
        // A hasher that finds another hash: the frame is a mismatch, and the view is never read.
        val other = ScriptedEffectHasher({ _, _, _, _ -> EffectResult.Hash("sha256:" + "1".repeat(64)) }, { _, _ -> error("not read") })
        assertEquals(ShownEffect.Verdict.Invalid(Validity.EFFECT_MISMATCH), ShownEffect.verify(a, other))
        assertTrue(other.viewCalls.isEmpty())
        // The hasher refuses the op.
        val refused = ScriptedEffectHasher({ _, _, _, _ -> EffectResult.Failure("invalid_op", "float") }, { _, _ -> error("not read") })
        assertEquals(ShownEffect.Verdict.Invalid(Validity.INVALID_OP), ShownEffect.verify(a, refused))
        // The view fails with the code of the op.
        val viewOp = ScriptedEffectHasher({ _, _, _, _ -> EffectResult.Hash(a.effect) }, { _, _ -> ViewResult.Failure("invalid_op", "x") })
        assertEquals(ShownEffect.Verdict.Invalid(Validity.INVALID_OP), ShownEffect.verify(a, viewOp))
    }

    @Test
    fun theHashThatTheAnswerCarriesIsComputedAgainFromTheStoredOp() {
        val repo = MemoryItemRepository()
        val a = Make.approval(Make.hex(1), Make.hex(2))
        val hasher = ReferenceEffectHasher()
        val engine = ItemEngine(repo, hasher, { 5L }, { "m-1" })
        engine.onRequest(1, a)
        assertEquals(1, hasher.effectCalls)
        val q = engine.approve(1, Make.hex(1)) as Submit.Queued
        // One more computation at the moment of the answer.
        assertEquals(2, hasher.effectCalls)
        val frame = q.entry.toFrame() as app.portalinbox.proto.AnswerFrame
        assertEquals(a.approval!!.effect, frame.shownEffect)
        // If the hash changes between the receipt and the answer (a native fault), the phone sends nothing.
        var calls = 0
        val flaky = ScriptedEffectHasher({ s, ac, r, op -> if (++calls == 1) hasher.effect(s, ac, r, op) else EffectResult.Hash("sha256:" + "2".repeat(64)) }, { op, v -> hasher.view(op, v) })
        val engine2 = ItemEngine(MemoryItemRepository(), flaky, { 5L })
        engine2.onRequest(1, a)
        assertEquals(Submit.Refused(Refusal.INVALID_REQUEST), engine2.approve(1, Make.hex(1)))
    }

    @Test
    fun everyVectorOfTheRuleGivesTheSameHashInKotlinAsInRust() {
        val file = StrictJson.parseObject(Fixtures.text("effect-vectors.json"))
        val vectors = file.getJSONArray("vectors")
        val hasher = ReferenceEffectHasher()
        var v01 = 0
        var v02 = 0
        for (n in 0 until vectors.length()) {
            val v = vectors.getJSONObject(n)
            val input = v.getJSONObject("input")
            val op = input.get("op")
            val result = hasher.effect(input.getString("service"), input.getString("action"), input.getString("resource"), StrictJson.compact(op))
            when (v.getString("group")) {
                "V01" -> {
                    v01++
                    assertEquals(v.getString("id"), v.getString("effect"), (result as EffectResult.Hash).effect)
                    assertEquals(v.getString("id"), v.getString("canonical"), hasher.canonical(input.getString("service"), input.getString("action"), input.getString("resource"), op as JSONObject))
                }
                "V02" -> {
                    v02++
                    assertEquals(v.getString("id"), "invalid_op", (result as EffectResult.Failure).code)
                }
                else -> error("unknown group")
            }
        }
        assertTrue("$v01 valid vectors", v01 >= 14)
        assertTrue("$v02 invalid vectors", v02 >= 9)
    }
}
