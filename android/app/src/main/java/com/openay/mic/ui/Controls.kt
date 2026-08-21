package com.openay.mic.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.openay.mic.logic.formatCount
import com.openay.mic.logic.formatUptime
import com.openay.mic.ui.theme.Amber
import com.openay.mic.ui.theme.Cream
import com.openay.mic.ui.theme.DataPlex
import com.openay.mic.ui.theme.Dim
import com.openay.mic.ui.theme.DisplayChakra
import com.openay.mic.ui.theme.Ink
import com.openay.mic.ui.theme.Line
import com.openay.mic.ui.theme.OpenAyDataStyles
import com.openay.mic.ui.theme.Panel
import com.openay.mic.ui.theme.Tally
import com.openay.mic.ui.theme.TallyContainer
import com.openay.mic.ui.theme.TallySoft

/** Single-select chip descriptor. */
data class ChipOption(val label: String, val value: String)

/** Wordmark lamp dot: tally while running, line when idle. */
@Composable
fun LampDot(running: Boolean, modifier: Modifier = Modifier) {
    Box(
        modifier
            .size(8.dp)
            .then(
                if (running) {
                    Modifier.shadow(9.dp, CircleShape, ambientColor = Tally.copy(alpha = 0.4f), spotColor = Tally.copy(alpha = 0.75f))
                } else {
                    Modifier
                },
            )
            .background(if (running) Tally else Line, CircleShape),
    )
}

/** Tally-tinted error row (last_error / native load failure). */
@Composable
fun ErrorPanel(message: String, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier
            .clip(RoundedCornerShape(2.dp))
            .background(TallyContainer)
            .border(1.dp, Tally.copy(alpha = 0.55f), RoundedCornerShape(2.dp))
            .padding(horizontal = 10.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(7.dp).background(Tally, CircleShape))
        Text(
            message,
            style = OpenAyDataStyles.small,
            color = TallySoft,
            maxLines = 3,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
    }
}

/**
 * TRANSPORT segmented control: WI-FI -> udp, USB -> tcp (adb), BT disabled
 * with a SOON tag — dim, not invisible.
 */
@Composable
fun TransportSegments(selected: String, onSelect: (String) -> Unit, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier
            .clip(RoundedCornerShape(2.dp))
            .border(1.dp, Line, RoundedCornerShape(2.dp))
            .background(Panel)
            .padding(3.dp),
        horizontalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        TransportSegment("WI-FI", selected = selected == "udp", enabled = true, onClick = { onSelect("udp") }, modifier = Modifier.weight(1f))
        TransportSegment("USB", selected = selected == "tcp", enabled = true, onClick = { onSelect("tcp") }, modifier = Modifier.weight(1f))
        TransportSegment("BT", selected = false, enabled = false, onClick = {}, modifier = Modifier.weight(1f))
    }
}

@Composable
private fun TransportSegment(
    label: String,
    selected: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Row(
        modifier = modifier
            .clip(RoundedCornerShape(2.dp))
            .background(if (selected) Amber.copy(alpha = 0.10f) else Color.Transparent)
            .then(if (selected) Modifier.border(1.dp, Amber, RoundedCornerShape(2.dp)) else Modifier)
            .clickable(enabled = enabled) { onClick() }
            .padding(vertical = 8.dp),
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            color = when {
                !enabled -> Dim.copy(alpha = 0.7f)
                selected -> Amber
                else -> Dim
            },
        )
        if (!enabled) {
            Text(
                "SOON",
                style = OpenAyDataStyles.micro.copy(fontSize = 7.sp, lineHeight = 9.sp),
                color = Dim.copy(alpha = 0.8f),
                modifier = Modifier
                    .padding(start = 5.dp)
                    .border(1.dp, Line, RoundedCornerShape(2.dp))
                    .padding(horizontal = 3.dp, vertical = 1.dp),
            )
        }
    }
}

/** Single-select chip row (CODEC / FRAME). */
@Composable
fun ChipRow(
    options: List<ChipOption>,
    selected: String,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    Row(modifier = modifier, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        options.forEach { option ->
            SelectChip(
                option = option,
                selected = option.value == selected,
                onClick = { onSelect(option.value) },
            )
        }
    }
}

@Composable
private fun SelectChip(option: ChipOption, selected: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .clip(RoundedCornerShape(2.dp))
            .background(if (selected) Amber.copy(alpha = 0.10f) else Panel)
            .border(1.dp, if (selected) Amber else Line, RoundedCornerShape(2.dp))
            .clickable { onClick() }
            .padding(horizontal = 14.dp, vertical = 7.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            chipLabel(option.label),
            style = MaterialTheme.typography.labelMedium,
            color = if (selected) Amber else Dim,
        )
    }
}

/** Digits in Plex Mono, remainder in Chakra (e.g. "5 MS" -> mono 5 + Chakra MS). */
private fun chipLabel(text: String): AnnotatedString {
    val m = Regex("^(\\d+)\\s+(.*)$").find(text)
    if (m == null) return AnnotatedString(text)
    return buildAnnotatedString {
        withStyle(SpanStyle(fontFamily = DataPlex, fontWeight = FontWeight.Medium)) { append(m.groupValues[1]) }
        append(" ")
        withStyle(SpanStyle(fontFamily = DisplayChakra, fontWeight = FontWeight.Medium)) { append(m.groupValues[2]) }
    }
}

/** NETWORK card: HOST/PORT fields + live SENT/LOST/UP readout (Plex Mono). */
@Composable
fun NetworkCard(
    host: String,
    onHostChange: (String) -> Unit,
    port: String,
    onPortChange: (String) -> Unit,
    sent: Long,
    lost: Int,
    uptimeSeconds: Long,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .clip(RoundedCornerShape(2.dp))
            .background(Panel)
            .border(1.dp, Line, RoundedCornerShape(2.dp))
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text("NETWORK", style = MaterialTheme.typography.labelSmall, color = Dim.copy(alpha = 0.9f))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                value = host,
                onValueChange = onHostChange,
                label = { Text("HOST", style = MaterialTheme.typography.labelSmall) },
                singleLine = true,
                shape = RoundedCornerShape(2.dp),
                textStyle = OpenAyDataStyles.regular,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Text),
                colors = fieldColors(),
                modifier = Modifier.weight(1f),
            )
            OutlinedTextField(
                value = port,
                onValueChange = onPortChange,
                label = { Text("PORT", style = MaterialTheme.typography.labelSmall) },
                singleLine = true,
                shape = RoundedCornerShape(2.dp),
                textStyle = OpenAyDataStyles.regular,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                colors = fieldColors(),
                modifier = Modifier.width(112.dp),
            )
        }
        val readout = buildAnnotatedString {
            append("SENT ${formatCount(sent)} · ")
            withStyle(SpanStyle(color = if (lost > 0) Amber else Cream)) { append("$lost LOST") }
            append(" · UP ${formatUptime(uptimeSeconds)}")
        }
        Text(
            readout,
            style = OpenAyDataStyles.small,
            color = Cream,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun fieldColors() = OutlinedTextFieldDefaults.colors(
    focusedBorderColor = Amber,
    unfocusedBorderColor = Line,
    focusedLabelColor = Amber,
    unfocusedLabelColor = Dim,
    cursorColor = Amber,
    focusedTextColor = Cream,
    unfocusedTextColor = Cream,
    focusedContainerColor = Ink,
    unfocusedContainerColor = Ink,
    disabledContainerColor = Ink,
)

/** Caption + content group (TRANSPORT / CODEC / FRAME rows). */
@Composable
fun SettingsRow(label: String, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(label, style = MaterialTheme.typography.labelSmall, color = Dim.copy(alpha = 0.9f))
        content()
    }
}