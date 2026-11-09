package app.portalinbox.testsupport

import app.portalinbox.proto.AgentFrame
import app.portalinbox.proto.AgentResultFrame
import app.portalinbox.proto.AnswerResultFrame
import app.portalinbox.proto.DigestFrame
import app.portalinbox.proto.MessageFrame
import app.portalinbox.proto.RequestFrame
import app.portalinbox.proto.ServerFrames
import app.portalinbox.proto.SettledFrame
import app.portalinbox.proto.StrictJson
import app.portalinbox.proto.WelcomeFrame
import org.json.JSONObject

/** Builds frames of the server from the golden files, with the ids and the values that a test needs. */
object Make {
    /** An id of 32 hexadecimal characters from a number. */
    fun hex(n: Int): String = n.toString(16).padStart(32, '0')

    private val effects = ReferenceEffectHasher()

    private fun golden(name: String): JSONObject = Fixtures.golden("server", name).json

    /**
     * An approval. The `effect` is the hash of the content, so the phone finds the request valid,
     * unless `effect` is given.
     */
    fun approval(
        requestId: String,
        deliveryId: String,
        seq: Long = 0,
        sql: String = "INSERT INTO analytics.events SELECT 1",
        createdAtMs: Long = 1_788_000_000_000L,
        effect: String? = null,
        allowCustom: Boolean = true,
        allowAskBack: Boolean = true,
        edit: (JSONObject) -> Unit = {},
    ): RequestFrame {
        val o = golden("request-approval.json")
        o.put("id", deliveryId).put("request_id", requestId).put("chain_id", requestId).put("seq", seq)
            .put("created_at_ms", createdAtMs).put("allow_custom", allowCustom).put("allow_ask_back", allowAskBack)
        val a = o.getJSONObject("approval")
        a.getJSONObject("op").put("sql", sql)
        a.put(
            "effect",
            effect ?: (effects.effect(a.getString("service"), a.getString("action"), a.getString("resource"), StrictJson.compact(a.getJSONObject("op")))
                as app.portalinbox.core.EffectResult.Hash).effect,
        )
        edit(o)
        return ServerFrames.decode(StrictJson.compact(o)) as RequestFrame
    }

    fun question(
        requestId: String,
        deliveryId: String,
        seq: Long = 0,
        file: String = "request-question-choice.json",
        createdAtMs: Long = 1_788_000_000_000L,
        edit: (JSONObject) -> Unit = {},
    ): RequestFrame {
        val o = golden(file)
        o.put("id", deliveryId).put("request_id", requestId).put("chain_id", requestId).put("seq", seq).put("created_at_ms", createdAtMs)
        edit(o)
        return ServerFrames.decode(StrictJson.compact(o)) as RequestFrame
    }

    fun message(requestId: String, deliveryId: String, seq: Long, user: Boolean): MessageFrame {
        val o = golden(if (user) "message-user.json" else "message-agent.json")
        o.put("id", deliveryId).put("request_id", requestId).put("seq", seq)
        return ServerFrames.decode(StrictJson.compact(o)) as MessageFrame
    }

    fun settled(requestId: String, deliveryId: String, file: String = "settled-answered.json", atMs: Long = 1_788_000_300_000L): SettledFrame {
        val o = golden(file)
        o.put("id", deliveryId).put("request_id", requestId).put("at_ms", atMs)
        return ServerFrames.decode(StrictJson.compact(o)) as SettledFrame
    }

    fun digest(deliveryId: String): DigestFrame {
        val o = golden("digest.json").put("id", deliveryId)
        return ServerFrames.decode(StrictJson.compact(o)) as DigestFrame
    }

    fun result(requestId: String, clientMsgId: String, status: String, state: String? = null): AnswerResultFrame {
        val o = JSONObject().put("t", "answer_result").put("request_id", requestId).put("client_msg_id", clientMsgId).put("status", status)
        if (state != null) o.put("state", state)
        return ServerFrames.decode(StrictJson.compact(o)) as AnswerResultFrame
    }

    fun welcome(file: String = "welcome.json"): WelcomeFrame = ServerFrames.decode(Fixtures.golden("server", file).text) as WelcomeFrame

    /** The ids of the agents that the tests use. */
    fun agentId(n: Int): String = hex(0xA000 + n)

    /**
     * An `agent` frame from the golden file of the state ([file]: `agent-known.json`, `agent-new.json`...). The members that the
     * test sets are put over the golden values. `merged_into` is added by [edit].
     */
    fun agent(
        agentId: String,
        deliveryId: String,
        rev: Long = 1,
        file: String = "agent-known.json",
        edit: (JSONObject) -> Unit = {},
    ): AgentFrame {
        val o = golden(file)
        o.put("id", deliveryId).put("agent_id", agentId).put("rev", rev)
        edit(o)
        return ServerFrames.decode(StrictJson.compact(o)) as AgentFrame
    }

    fun agentResult(clientMsgId: String, agentId: String, status: String = "ok"): AgentResultFrame {
        val o = JSONObject().put("t", "agent_result").put("client_msg_id", clientMsgId).put("agent_id", agentId).put("status", status)
        return ServerFrames.decode(StrictJson.compact(o)) as AgentResultFrame
    }

    /** Puts a request into the group of an agent: `from.agent_id`, `from.label`, the kind and the state of the agent. */
    fun asAgent(
        o: JSONObject, agentId: String, label: String, state: String = "known", kind: String = "agent", keyName: String = "ops-key",
    ) {
        o.getJSONObject("from").put("agent_id", agentId).put("label", label).put("agent_state", state).put("agent_kind", kind).put("key_name", keyName)
    }
}
