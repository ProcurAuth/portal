package app.portalinbox.service

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** The lines of the snackbar after an `agent_result`: each line shows once, in order, and an old one is dropped. */
class AgentNoticesTest {
    @After
    fun clear() = AgentNotices.clear()

    @Test
    fun aLineIsTakenOnceInTheOrderOfArrival() {
        AgentNotices.clear()
        AgentNotices.post("one")
        AgentNotices.post("two")
        assertEquals("one", AgentNotices.take())
        assertEquals("two", AgentNotices.take())
        assertNull(AgentNotices.take())
    }

    @Test
    fun noScreenOpenKeepsOnlyTheLastLines() {
        AgentNotices.clear()
        for (n in 1..AgentNotices.MAX_KEPT + 5) AgentNotices.post("line $n")
        val taken = generateSequence { AgentNotices.take() }.toList()
        assertEquals(AgentNotices.MAX_KEPT, taken.size)
        assertEquals("line ${AgentNotices.MAX_KEPT + 5}", taken.last())
        assertEquals("line 6", taken.first())
    }
}
