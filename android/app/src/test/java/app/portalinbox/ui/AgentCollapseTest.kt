package app.portalinbox.ui

import app.portalinbox.data.AgentKey
import app.portalinbox.testsupport.Make
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/** The folded groups: local to the phone, a set of keys that a text of the preferences can hold. */
class AgentCollapseTest {
    private val k1 = AgentKey.of(1, Make.agentId(1))
    private val k2 = AgentKey.of(2, Make.agentId(1))

    @Test
    fun aTapFoldsAndAnotherTapOpens() {
        val c = AgentCollapse()
        assertFalse(c.isFolded(k1))
        val folded = c.toggle(k1)
        assertTrue(folded.isFolded(k1))
        assertFalse(folded.isFolded(k2))
        assertFalse(folded.toggle(k1).isFolded(k1))
    }

    @Test
    fun theStateRoundTripsThroughTheTextOfThePreferences() {
        val c = AgentCollapse().toggle(k1).toggle(k2)
        assertEquals(c, AgentCollapse.decode(c.encode()))
        assertEquals(AgentCollapse(), AgentCollapse.decode(AgentCollapse().encode()))
        assertEquals(AgentCollapse(), AgentCollapse.decode(null))
    }

    @Test
    fun aTextThatIsNotAKeyIsDroppedWhenReadFromThePreferences() {
        val text = "$k1,garbage,1:XYZ,:" + Make.agentId(2) + ",$k2,12:" + "a".repeat(31)
        assertEquals(setOf(k1, k2), AgentCollapse.decode(text).folded)
    }

    @Test
    fun theStateNeverHoldsMoreThanTheLimit() {
        val many = (0 until AgentCollapse.MAX_KEYS + 50).joinToString(",") { AgentKey.of(it.toLong(), Make.agentId(it)) }
        assertEquals(AgentCollapse.MAX_KEYS, AgentCollapse.decode(many).folded.size)
    }

    @Test
    fun aGroupThatNoLongerExistsLeavesTheState() {
        val c = AgentCollapse().toggle(k1).toggle(k2)
        val kept = c.retain(listOf(k2))
        assertEquals(setOf(k2), kept.folded)
        assertSame(c, c.retain(listOf(k1, k2)))
    }
}
