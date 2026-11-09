package app.portalinbox.alert

import app.portalinbox.alert.Attention.Rank
import app.portalinbox.alert.Attention.Sound
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.proto.Urgency
import app.portalinbox.service.LinkState
import app.portalinbox.testsupport.AttentionWorld
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The aggregate of the one notification: counts, lines, ages, the single approval, the alert-again rule and the plan. */
class AttentionTest {
    private val w = AttentionWorld()
    private val minute = 60_000L
    private val hour = 60 * minute

    private fun compute(input: Attention.Input) = Attention.compute(input)

    // ------------------------------------------------------------ counts and portal lines

    @Test
    fun oneNotificationHoldsTheCountsAndTheOldestAge() {
        val a = compute(
            w.input(
                items = listOf(w.approval(ageMs = 3 * hour), w.approval(ageMs = hour), w.question(ageMs = minute)),
                alerts = listOf(w.alert()),
            ),
        )
        assertEquals(2, a.approvals)
        assertEquals(1, a.questions)
        assertEquals(1, a.alerts)
        assertEquals(3 * hour, a.oldestAgeMs)
        val t = AttentionText.of(a)
        assertEquals("3 to answer, 1 alert", t.title)
        assertEquals("Production · The oldest waits 3 hours", t.text)
        // One portal that is up: the two lines say it all.
        assertTrue(t.lines.isEmpty())
    }

    @Test
    fun severalPortalsGiveOneLineForEachPortal() {
        val a = compute(
            w.input(
                portals = listOf(w.portal(w.production), w.portal(w.homelab)),
                items = listOf(w.approval(1), w.question(1), w.question(2)),
                alerts = listOf(w.alert(1)),
            ),
        )
        val t = AttentionText.of(a)
        assertEquals("3 to answer, 1 alert", t.title)
        assertEquals(listOf("Production: 2 to answer, 1 alert", "Homelab: 1 to answer"), t.lines)
        assertTrue(t.text.startsWith("Production, Homelab · "))
    }

    @Test
    fun aPortalWithNothingToSayHasNoLine() {
        val a = compute(w.input(portals = listOf(w.portal(w.production), w.portal(w.homelab)), items = listOf(w.question(2))))
        assertEquals(listOf("Homelab"), a.lines.map { it.identity.nickname })
    }

    @Test
    fun theOldestAgeUsesTheClockOfEachPortal() {
        val skewed = Attention.Portal(w.homelab, false, false, null, w.now + 2 * hour)
        val a = compute(w.input(portals = listOf(w.portal(w.production), skewed), items = listOf(w.question(1, ageMs = hour), w.question(2, ageMs = hour))))
        // The Homelab clock is two hours ahead: its item is three hours old on its own clock.
        assertEquals(3 * hour, a.oldestAgeMs)
        assertEquals(hour, a.lines[0].oldestAgeMs)
    }

    @Test
    fun repliesCountOnlyForAnOpenItem() {
        val item = w.question()
        val a = compute(w.input(items = listOf(item), replies = listOf(Attention.Reply(1, item.requestId, Urgency.NORMAL), Attention.Reply(1, "gone", Urgency.NORMAL))))
        assertEquals(1, a.replies)
        assertEquals("1 to answer, 1 reply", AttentionText.of(a).title)
    }

    @Test
    fun anAlertThatIsReadOrOffMakesNothing() {
        val a = compute(w.input(alerts = listOf(w.alert(readAtMs = 5L), w.alert(action = "off"))))
        assertTrue(a.isEmpty)
        assertEquals(0, a.alerts)
    }

    @Test
    fun aThingOfARemovedPortalDoesNotCount() {
        val a = compute(w.input(portals = listOf(w.portal(w.production)), items = listOf(w.question(9)), alerts = listOf(w.alert(9))))
        assertTrue(a.isEmpty)
    }

    @Test
    fun aClosedItemDoesNotCount() {
        val i = w.question()
        val closed = i.copy(state = app.portalinbox.proto.ItemState.ANSWERED)
        assertTrue(compute(w.input(items = listOf(closed))).isEmpty)
    }

    // ------------------------------------------------------------ lock-screen text

    @Test
    fun noTextOfAnyVersionHoldsABodyACommandOrAnAgentText() {
        val approval = w.approval()
        val question = w.question()
        val t = AttentionText.of(compute(w.input(portals = listOf(w.portal(w.production), w.portal(w.homelab)), items = listOf(approval, question, w.question(2)), alerts = listOf(w.alert()))))
        val all = listOf(t.title, t.text, t.publicTitle, t.publicText) + t.lines
        val secrets = listOf(
            "INSERT", "SELECT", "analytics", "claude-laptop", "datastore", "K7QF", "Purge", "Disk full", "infra", "key ", "sha256", "ClickHouse",
            question.request.question?.title ?: "x", approval.request.from.label,
        )
        for (s in secrets) for (text in all) assertFalse("'$s' in '$text'", text.contains(s, ignoreCase = true))
    }

    @Test
    fun theLockScreenNamesThePortalsUnlessTheUserHidIt() {
        val a = compute(w.input(portals = listOf(w.portal(w.production), w.portal(w.homelab, hide = true)), items = listOf(w.question(1), w.question(2))))
        val t = AttentionText.of(a)
        assertEquals("2 to answer", t.publicTitle)
        assertEquals("Production · Unlock to see them.", t.publicText)
        // The unlocked version names both.
        assertTrue(t.text.startsWith("Production, Homelab"))
        assertTrue(t.lines.any { it.startsWith("Homelab:") })
    }

    @Test
    fun whenEveryPortalIsHiddenTheLockScreenNamesNone() {
        val a = compute(w.input(portals = listOf(w.portal(w.production, hide = true)), items = listOf(w.question(1))))
        val t = AttentionText.of(a)
        assertEquals("Unlock to see them.", t.publicText)
        assertFalse(t.publicTitle.contains("Production"))
        assertFalse(t.publicText.contains("Production"))
    }

    @Test
    fun aNicknameGoesThroughTheEscapeOfHiddenCharacters() {
        val odd = w.production.copy(nickname = "Pr‮od")
        val t = AttentionText.of(compute(w.input(portals = listOf(w.portal(odd)), items = listOf(w.question(1)))))
        assertFalse(t.text.contains('‮'))
        assertTrue(t.text.contains("Pr<U+202E>od"))
    }

    // ------------------------------------------------------------ portals that are down

    private val offline = Attention.Down(Attention.DownKind.OFFLINE, 0L)

    @Test
    fun aPortalThatIsDownIsALineOfTheSameNotification() {
        val a = compute(
            w.input(
                portals = listOf(w.portal(w.production), w.portal(w.homelab, down = Attention.Down(Attention.DownKind.OFFLINE, w.now - 3 * hour))),
                items = listOf(w.question(1)),
            ),
        )
        val t = AttentionText.of(a)
        assertEquals("1 to answer", t.title)
        assertEquals(listOf("Production: 1 to answer", "Homelab: not connected for 3 hours"), t.lines)
        assertTrue(t.text.endsWith("Homelab is not connected"))
    }

    @Test
    fun onlyWarningsGiveATitleWithTheNameOrNot() {
        val one = AttentionText.of(compute(w.input(portals = listOf(w.portal(w.homelab, down = offline)))))
        assertEquals("Homelab is not connected", one.title)
        assertEquals("Homelab is not connected", one.publicTitle)
        val hidden = AttentionText.of(compute(w.input(portals = listOf(w.portal(w.homelab, hide = true, down = offline)))))
        assertEquals("Homelab is not connected", hidden.title)
        assertEquals("A portal is not connected", hidden.publicTitle)
        val two = AttentionText.of(compute(w.input(portals = listOf(w.portal(w.production, down = offline), w.portal(w.homelab, down = offline)))))
        assertEquals("2 portals are not connected", two.title)
        assertEquals(2, two.lines.size)
    }

    @Test
    fun withNoNetworkNoPortalWarns() {
        val a = compute(w.input(portals = listOf(w.portal(w.production, down = offline), w.portal(w.homelab, down = offline)), network = false))
        assertTrue(a.isEmpty)
        assertEquals(0, a.down)
    }

    @Test
    fun theRuleOfTheWarningFollowsTheLinkState() {
        val warn = 180_000L
        assertNull(Attention.downOf(LinkState.CONNECTED, 0, warn, 10 * hour))
        assertNull(Attention.downOf(LinkState.CONNECTING, 0, warn, 10 * hour))
        assertNull(Attention.downOf(LinkState.DISCONNECTED, 0, warn, warn - 1))
        assertEquals(Attention.DownKind.OFFLINE, Attention.downOf(LinkState.DISCONNECTED, 0, warn, warn)?.kind)
        assertEquals(Attention.DownKind.REENROLL, Attention.downOf(LinkState.REENROLL, 5, warn, 6)?.kind)
        assertEquals(Attention.DownKind.KEY_CHANGED, Attention.downOf(LinkState.KEY_CHANGED, 5, warn, 6)?.kind)
    }

    // ------------------------------------------------------------ the single approval

    @Test
    fun oneApprovalAndNothingElseOffersDecline() {
        val i = w.approval()
        val plan = plan(w.input(portals = listOf(w.portal(w.production), w.portal(w.homelab)), items = listOf(i)))
        assertEquals(1L to i.requestId, plan.decline)
    }

    @Test
    fun anythingElseRemovesTheDeclineAction() {
        val a = w.approval()
        assertNull(plan(w.input(items = listOf(a, w.approval()))).decline)
        assertNull(plan(w.input(items = listOf(a, w.question()))).decline)
        assertNull(plan(w.input(items = listOf(a), alerts = listOf(w.alert()))).decline)
        assertNull(plan(w.input(items = listOf(a), replies = listOf(Attention.Reply(1, a.requestId, Urgency.NORMAL)))).decline)
        assertNull(plan(w.input(portals = listOf(w.portal(w.production), w.portal(w.homelab, down = offline)), items = listOf(a))).decline)
        assertNull(plan(w.input(items = listOf(w.question()))).decline)
    }

    // ------------------------------------------------------------ alert again

    private fun decide(before: Attention.Aggregate?, now: Attention.Aggregate) =
        Attention.decide(before?.entries?.associate { it.key to it.rank }, now)

    @Test
    fun aNewRequestAlertsAndASecondOneAlertsToo() {
        val a1 = w.approval()
        val first = compute(w.input(items = listOf(a1)))
        assertEquals(Sound.RING, Attention.decide(emptyMap(), first).sound)
        val a2 = w.approval()
        val second = compute(w.input(items = listOf(a1, a2)))
        val d = decide(first, second)
        assertTrue(d.alert)
        assertEquals(Sound.RING, d.sound)
    }

    @Test
    fun nothingNewDoesNotAlert() {
        val a1 = w.approval()
        val first = compute(w.input(items = listOf(a1)))
        val d = decide(first, first)
        assertFalse(d.alert)
        assertFalse(d.arrivals)
    }

    @Test
    fun aNewLowItemUpdatesInSilence() {
        val normal = w.approval(urgency = Urgency.NORMAL)
        val first = compute(w.input(items = listOf(normal)))
        val low = w.question(urgency = Urgency.LOW)
        val d = decide(first, compute(w.input(items = listOf(normal, low))))
        assertFalse(d.alert)
        assertTrue(d.arrivals)
    }

    @Test
    fun aLessUrgentArrivalUnderAMoreUrgentOneIsSilentAndAMoreUrgentOneAlerts() {
        val high = w.approval(urgency = Urgency.HIGH)
        val first = compute(w.input(items = listOf(high)))
        val normal = w.question(urgency = Urgency.NORMAL)
        assertFalse(decide(first, compute(w.input(items = listOf(high, normal)))).alert)
        val high2 = w.approval(urgency = Urgency.HIGH)
        assertTrue(decide(first, compute(w.input(items = listOf(high, high2)))).alert)
        val alarm = w.alert(action = "alarm")
        val d = decide(first, compute(w.input(items = listOf(high), alerts = listOf(alarm))))
        assertTrue(d.alert)
        assertEquals(Sound.ALARM, d.sound)
    }

    @Test
    fun aNewHighItemAlertsOverNormalOnes() {
        val normal = w.question()
        val first = compute(w.input(items = listOf(normal)))
        val high = w.approval(urgency = Urgency.HIGH)
        assertTrue(decide(first, compute(w.input(items = listOf(normal, high)))).alert)
    }

    @Test
    fun aThingThatLeftDoesNotHoldTheLevelUp() {
        val high = w.approval(urgency = Urgency.HIGH)
        val normal = w.question()
        val first = compute(w.input(items = listOf(high, normal)))
        // The high one is answered. A new normal item alerts again: only the normal one stays.
        val n2 = w.question()
        assertTrue(decide(first, compute(w.input(items = listOf(normal, n2)))).alert)
    }

    @Test
    fun anUpdateOfARequestOfTheSameChainIsNotANewArrival() {
        val i = w.approval()
        val first = compute(w.input(items = listOf(i)))
        // A newer request of the chain has another request id but the same chain id: the key is the chain.
        val newer = i.copy(request = i.request.copy(requestId = "f".repeat(32), seq = 1))
        val d = decide(first, compute(w.input(items = listOf(newer))))
        assertFalse(d.alert)
        assertFalse(d.arrivals)
    }

    @Test
    fun theThingsThatThePhoneAlreadyNotifiedBeforeARestartAreNotNew() {
        val seen = w.seen(w.approval())
        val fresh = w.approval()
        val a = compute(w.input(items = listOf(seen, fresh)))
        // After a restart nothing is known: the notified item counts as shown, the other one as new.
        val d = Attention.decide(null, a)
        assertTrue(d.alert)
        val onlySeen = compute(w.input(items = listOf(seen)))
        assertFalse(Attention.decide(null, onlySeen).alert)
    }

    @Test
    fun aRingAlertAlertsOverNormalItemsWithTheRingSound() {
        val normal = w.question()
        val first = compute(w.input(items = listOf(normal)))
        val d = decide(first, compute(w.input(items = listOf(normal), alerts = listOf(w.alert(action = "ring")))))
        assertTrue(d.alert)
        assertEquals(Sound.RING, d.sound)
    }

    @Test
    fun aNotifyAlertUsesTheSoundOfThePhone() {
        val d = Attention.decide(emptyMap(), compute(w.input(alerts = listOf(w.alert(action = "notify")))))
        assertTrue(d.alert)
        assertEquals(Sound.PHONE, d.sound)
    }

    // ------------------------------------------------------------ quiet window

    private val paris = "Europe/Paris"
    private val night = QuietPrefs("22:00", "07:00", paris, highRings = false)
    private val loud = QuietPrefs("22:00", "07:00", paris, highRings = true)
    private val inWindow = java.time.LocalDateTime.parse("2026-11-08T23:00:00").atZone(java.time.ZoneId.of(paris)).toInstant().toEpochMilli()
    private val outside = java.time.LocalDateTime.parse("2026-11-08T09:00:00").atZone(java.time.ZoneId.of(paris)).toInstant().toEpochMilli()

    private fun at(ms: Long, quiet: QuietPrefs?, items: List<app.portalinbox.data.Item>, muted: Boolean = false) =
        Attention.compute(
            Attention.Input(listOf(w.portal(w.production, quiet = quiet, muted = muted)), items, emptyList(), emptyList(), ms),
        )

    @Test
    fun aRequestInTheQuietWindowMakesNoSound() {
        val a = at(inWindow, night, listOf(w.approval(urgency = Urgency.NORMAL)))
        assertEquals(Sound.NONE, a.entries.single().sound)
        val d = Attention.decide(emptyMap(), a)
        assertFalse(d.alert)
        assertTrue(d.arrivals)
        assertEquals(Attention.CH_QUIET, Attention.plan(a, d, false, false).channel)
    }

    @Test
    fun aRequestOutsideTheWindowAlerts() {
        val a = at(outside, night, listOf(w.approval(urgency = Urgency.NORMAL)))
        assertTrue(Attention.decide(emptyMap(), a).alert)
    }

    @Test
    fun aHighRequestRingsInTheWindowOnlyWithHighRings() {
        val high = w.approval(urgency = Urgency.HIGH, quiet = true)
        assertTrue(Attention.decide(emptyMap(), at(inWindow, loud, listOf(high))).alert)
        assertFalse(Attention.decide(emptyMap(), at(inWindow, night, listOf(high))).alert)
        assertFalse(Attention.decide(emptyMap(), at(inWindow, null, listOf(high))).alert)
    }

    @Test
    fun theFlagQuietOfTheFrameSilencesWithoutPrefs() {
        val a = at(outside, null, listOf(w.approval(urgency = Urgency.NORMAL, quiet = true)))
        assertFalse(Attention.decide(emptyMap(), a).alert)
    }

    @Test
    fun aMutedPortalMakesNoSoundForItemsButItsAlarmStillRings() {
        val a = Attention.compute(w.input(portals = listOf(w.portal(w.production, muted = true)), items = listOf(w.approval()), alerts = listOf(w.alert(action = "alarm"), w.alert(action = "ring"))))
        assertEquals(Sound.NONE, a.entries.single { it.what == Attention.What.APPROVAL }.sound)
        // The alarm keeps its ring. The ring level is silent for a muted portal.
        assertEquals(setOf(Sound.ALARM, Sound.NONE), a.entries.filter { it.what == Attention.What.ALERT }.map { it.sound }.toSet())
        assertTrue(a.entries.any { it.sound == Sound.ALARM })
    }

    // ------------------------------------------------------------ plan: channel, tap target, alarm

    private fun plan(input: Attention.Input, shown: Boolean = false, direct: Boolean = false): Attention.Plan {
        val a = compute(input)
        return Attention.plan(a, Attention.decide(emptyMap(), a), direct, shown)
    }

    @Test
    fun aTapOpensTheHomeList() {
        assertEquals(Attention.Target.Home, plan(w.input(items = listOf(w.approval()))).target)
        assertEquals(Attention.Target.Home, plan(w.input(alerts = listOf(w.alert(action = "ring")))).target)
    }

    @Test
    fun anAlarmThatRingsOpensTheAlarmPageAndHasTheFullScreenIntentAndStop() {
        val alarm = w.alert(id = "al1", action = "alarm")
        val p = plan(w.input(alerts = listOf(alarm), items = listOf(w.approval())))
        assertEquals(Attention.Target.Alarm(1, "al1"), p.target)
        assertTrue(p.alarm && p.fullScreen && p.stopAlarm)
        assertEquals(Attention.CH_ATTENTION, p.channel)
        assertTrue(p.alert)
        assertNull(p.decline)
    }

    @Test
    fun anAlarmThatIsAlreadyOnScreenHasNoHeadsUpAndNoFullScreenIntent() {
        val p = plan(w.input(alerts = listOf(w.alert(action = "alarm"))), direct = true)
        assertEquals(Attention.CH_QUIET, p.channel)
        assertFalse(p.fullScreen)
        assertTrue(p.alarm && p.stopAlarm)
    }

    @Test
    fun aStoppedAlarmThatIsUnreadIsAnAlertWithoutTheAlarmFeatures() {
        val p = plan(w.input(alerts = listOf(w.alert(action = "alarm", stoppedAtMs = 5L))))
        assertFalse(p.alarm)
        assertFalse(p.stopAlarm)
        assertEquals(Attention.Target.Home, p.target)
        assertEquals("1 alert", p.content.title)
    }

    @Test
    fun theChannelIsChosenWhenTheNotificationIsPosted() {
        val loudInput = w.input(items = listOf(w.approval()))
        // A new thing that alerts: the channel that can alert.
        assertEquals(Attention.CH_ATTENTION, plan(loudInput).channel)
        // A silent thing and nothing on screen: quiet.
        val silent = w.input(items = listOf(w.question(urgency = Urgency.LOW)))
        assertEquals(Attention.CH_QUIET, plan(silent).channel)
        // Two channels and the status one: the number stays small.
        assertEquals(setOf("attention", "attention_quiet"), setOf(Attention.CH_ATTENTION, Attention.CH_QUIET))
    }

    @Test
    fun theColorOfThePortalIsUsedOnlyForOnePortal() {
        assertEquals(0, plan(w.input(items = listOf(w.question(1)))).colorIndex)
        val two = w.input(portals = listOf(w.portal(w.production), w.portal(w.homelab)), items = listOf(w.question(1), w.question(2)))
        assertNull(plan(two).colorIndex)
    }

    @Test
    fun theIdAndTheTagAreFixed() {
        assertEquals(3000, Attention.ID)
        assertEquals("attention", Attention.TAG)
        val p = plan(w.input(items = listOf(w.approval())))
        assertEquals(Attention.ID, p.id)
        assertEquals(Attention.TAG, p.tag)
        assertNotNull(p.content)
        assertEquals(Rank.NORMAL, Attention.compute(w.input(items = listOf(w.approval()))).maxRank)
    }
}
