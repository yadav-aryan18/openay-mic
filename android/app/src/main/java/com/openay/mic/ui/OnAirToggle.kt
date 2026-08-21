package com.openay.mic.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import com.openay.mic.ui.theme.Amber
import com.openay.mic.ui.theme.Cream
import com.openay.mic.ui.theme.Dim
import com.openay.mic.ui.theme.DisplayChakra
import com.openay.mic.ui.theme.Line
import com.openay.mic.ui.theme.Tally
import kotlinx.coroutines.launch

/**
 * Big circular ON AIR lamp (112 dp). COLD: engraved line ring + dim STANDBY.
 * HOT: amber glow ring, cream ON AIR, tally-red center dot. Press fires a
 * short ring pulse (~200 ms, no bounce) unless reduced motion is on.
 * Disabled (native library unavailable) is dimmed, not hidden.
 */
@Composable
fun OnAirToggle(
    running: Boolean,
    enabled: Boolean,
    reducedMotion: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    val pulse = remember { Animatable(1f) }

    Box(
        modifier = modifier
            .size(112.dp)
            .semantics {
                role = Role.Button
                contentDescription = if (running) "Stop capture" else "Start capture"
            }
            .clip(CircleShape)
            .background(if (running) Amber.copy(alpha = 0.06f) else Color.Transparent, CircleShape)
            .graphicsLayer {
                scaleX = pulse.value
                scaleY = pulse.value
                alpha = if (enabled) 1f else 0.55f
            }
            .clickable(enabled = enabled) {
                scope.launch {
                    if (!reducedMotion) {
                        pulse.snapTo(1.06f)
                        pulse.animateTo(1f, tween(durationMillis = 200, easing = FastOutSlowInEasing))
                    }
                }
                onClick()
            }
            .then(
                if (running && enabled) {
                    Modifier.shadow(
                        elevation = 18.dp,
                        shape = CircleShape,
                        ambientColor = Amber.copy(alpha = 0.22f),
                        spotColor = Amber.copy(alpha = 0.40f),
                    )
                } else {
                    Modifier
                },
            ),
        contentAlignment = Alignment.Center,
    ) {
        Canvas(Modifier.fillMaxSize().padding(8.dp)) {
            val stroke = if (running) 3.5.dp.toPx() else 2.5.dp.toPx()
            drawCircle(
                color = (if (running) Amber else Line).copy(alpha = if (enabled) 0.95f else 0.4f),
                style = Stroke(width = stroke),
            )
        }
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (running) {
                Box(
                    Modifier
                        .size(9.dp)
                        .shadow(10.dp, CircleShape, ambientColor = Tally.copy(alpha = 0.5f), spotColor = Tally.copy(alpha = 0.8f))
                        .background(Tally, CircleShape),
                )
            }
            Text(
                if (running) "ON AIR" else "STANDBY",
                fontFamily = DisplayChakra,
                fontWeight = if (running) FontWeight.SemiBold else FontWeight.Medium,
                fontSize = 14.sp,
                letterSpacing = 0.12.em,
                color = if (running) Cream else Dim,
            )
        }
    }
}