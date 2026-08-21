package com.openay.mic.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.openay.mic.logic.LinkDelta
import com.openay.mic.ui.theme.Amber
import com.openay.mic.ui.theme.Cream
import com.openay.mic.ui.theme.Dim
import com.openay.mic.ui.theme.Line
import com.openay.mic.ui.theme.OpenAyDataStyles
import com.openay.mic.ui.theme.Panel
import com.openay.mic.ui.theme.PanelRaised
import com.openay.mic.ui.theme.Tally
import kotlinx.coroutines.delay
import kotlin.math.roundToInt

private const val STAGES = 3
private const val STAGGER_DELAY_MS = 130L
private const val CLIP_LEVEL = 95f

/**
 * THE CHAIN — MIC ▸ LINK ▸ CONSOLE hero strip (design.md signature element).
 * Three stage cards joined by cable segments, each bound to real pipeline
 * state: the MIC ring tracks the live input level, LINK shows packet rate +
 * loss from consecutive stats deltas, CONSOLE shows the destination. Cables
 * pulse ~1 Hz while streaming; on connect the stages light left-to-right over
 * ~400 ms. Both are dropped when the system reduced-motion flag is set.
 */
@Composable
fun ChainCard(
    running: Boolean,
    level: Float,
    link: LinkDelta,
    consoleHost: String,
    consolePort: String,
    reducedMotion: Boolean,
    modifier: Modifier = Modifier,
) {
    // Power-on stagger: each stage lights 130 ms after the previous one.
    val lit = remember { List(STAGES) { Animatable(0f) } }
    LaunchedEffect(running, reducedMotion) {
        if (running) {
            if (reducedMotion) {
                lit.forEach { it.snapTo(1f) }
            } else {
                lit.forEach { it.snapTo(0f) }
                lit.forEachIndexed { i, a ->
                    if (i > 0) delay(STAGGER_DELAY_MS)
                    a.animateTo(1f, tween(durationMillis = 230, easing = LinearEasing))
                }
            }
        } else {
            lit.forEach { it.animateTo(0f, tween(durationMillis = 150, easing = LinearEasing)) }
        }
    }

    // ~1 Hz cable pulse while hot (value used only when streaming).
    val pulse by rememberInfiniteTransition(label = "cablePulse").animateFloat(
        initialValue = 0f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(tween(durationMillis = 1000, easing = LinearEasing), RepeatMode.Restart),
        label = "cablePulse",
    )

    Box(
        modifier = modifier
            .clip(RoundedCornerShape(2.dp))
            .background(Brush.verticalGradient(listOf(PanelRaised, Panel))) // subtle machined-metal shading
            .border(1.dp, Line, RoundedCornerShape(2.dp))
            .padding(12.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            StageCard(Modifier.weight(1f), "MIC", lit[0].value) { LevelRing(level) }
            CableSegment(hot = running, reducedMotion = reducedMotion, pulse = pulse)
            StageCard(Modifier.weight(1f), "LINK", lit[1].value) {
                Column(
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text("${link.sentPerSec}/s", style = OpenAyDataStyles.large, color = Cream)
                    Text(
                        "${link.lost} LOST",
                        style = OpenAyDataStyles.micro,
                        color = if (link.lost > 0) Amber else Dim,
                    )
                }
            }
            CableSegment(hot = running, reducedMotion = reducedMotion, pulse = pulse)
            StageCard(Modifier.weight(1f), "CONSOLE", lit[2].value) {
                Column(
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    // Host and port on separate lines: host:port in one string
                    // truncates on narrow screens and hides the port.
                    val hasTarget = consoleHost.isNotBlank() && consolePort.isNotBlank()
                    Text(
                        if (hasTarget) consoleHost else "—",
                        style = OpenAyDataStyles.small,
                        color = if (hasTarget) Cream else Dim,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        if (hasTarget) consolePort else "",
                        style = OpenAyDataStyles.micro,
                        color = Dim,
                    )
                }
            }
        }
    }
}

/** Instrument card: engraved caps label + live value, panel + hairline. */
@Composable
private fun StageCard(modifier: Modifier, label: String, lit: Float, content: @Composable () -> Unit) {
    Column(
        modifier = modifier
            .clip(RoundedCornerShape(2.dp))
            .background(Panel.copy(alpha = 0.85f))
            .border(1.dp, Line.copy(alpha = 0.8f), RoundedCornerShape(2.dp))
            .padding(horizontal = 8.dp, vertical = 10.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(label, style = MaterialTheme.typography.labelMedium, color = Dim.copy(alpha = 0.9f))
        // The value content dims with the stage ("cold" strip when idle).
        Box(Modifier.graphicsLayer { alpha = 0.25f + 0.75f * lit }) { content() }
    }
}

/**
 * Live input ring: line track, amber arc (smoothed level_peak), tally arc at
 * clip (>=95). The phone IS the MIC stage, so the ring moves whenever audio
 * flows, even before the link is up.
 */
@Composable
private fun LevelRing(level: Float, modifier: Modifier = Modifier) {
    Box(modifier.size(52.dp), contentAlignment = Alignment.Center) {
        Canvas(Modifier.fillMaxSize()) {
            val stroke = 4.dp.toPx()
            val inset = stroke / 2f
            val arcSize = Size(size.width - stroke, size.height - stroke)
            val topLeft = Offset(inset, inset)
            drawArc(
                color = Line,
                startAngle = -90f,
                sweepAngle = 360f,
                useCenter = false,
                topLeft = topLeft,
                size = arcSize,
                style = Stroke(width = stroke, cap = StrokeCap.Butt),
            )
            val clamped = (level / 100f).coerceIn(0f, 1f)
            if (clamped > 0f) {
                drawArc(
                    color = if (level >= CLIP_LEVEL) Tally else Amber,
                    startAngle = -90f,
                    sweepAngle = 360f * clamped,
                    useCenter = false,
                    topLeft = topLeft,
                    size = arcSize,
                    style = Stroke(width = stroke, cap = StrokeCap.Round),
                )
            }
        }
        Text("${level.roundToInt()}", style = OpenAyDataStyles.micro, color = Cream.copy(alpha = 0.5f))
    }
}

/** Cable segment joining stages: amber pulse while hot, dim line when idle. */
@Composable
private fun CableSegment(hot: Boolean, reducedMotion: Boolean, pulse: Float, modifier: Modifier = Modifier) {
    val alpha = when {
        !hot -> 0.22f
        reducedMotion -> 0.55f
        else -> 0.35f + 0.5f * pulse
    }
    Box(
        modifier
            .width(14.dp)
            .height(2.dp)
            .background((if (hot) Amber else Line).copy(alpha = alpha), RoundedCornerShape(1.dp)),
    )
}