package com.openay.mic.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
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
import androidx.compose.material3.AlertDialog
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
import com.openay.mic.BluetoothLink
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
 * TRANSPORT segmented control: WI-FI -> udp, USB -> tcp (adb), BT -> the
 * Bluetooth RFCOMM (SPP) link to the console.
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
        TransportSegment("WI-FI", selected = selected == "udp", onClick = { onSelect("udp") }, modifier = Modifier.weight(1f))
        TransportSegment("USB", selected = selected == "tcp", onClick = { onSelect("tcp") }, modifier = Modifier.weight(1f))
        TransportSegment("BT", selected = selected == "bt", onClick = { onSelect("bt") }, modifier = Modifier.weight(1f))
    }
}

@Composable
private fun TransportSegment(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Row(
        modifier = modifier
            .clip(RoundedCornerShape(2.dp))
            .background(if (selected) Amber.copy(alpha = 0.10f) else Color.Transparent)
            .then(if (selected) Modifier.border(1.dp, Amber, RoundedCornerShape(2.dp)) else Modifier)
            .clickable { onClick() }
            .padding(vertical = 8.dp),
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            color = if (selected) Amber else Dim,
        )
    }
}

/** BT DEVICE chip: shows the picked device (or a prompt), opens the picker. */
@Composable
fun BtDeviceChip(label: String, selected: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier) {
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
            label,
            style = MaterialTheme.typography.labelMedium,
            color = if (selected) Amber else Dim,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/**
 * BT DEVICE picker: bonded devices as rack rows (panel + hairline idiom, amber
 * when selected); picking persists and closes. The empty state prompts pairing
 * the console in system settings.
 */
@Composable
fun BtDevicePickerDialog(
    devices: List<BluetoothLink.BondedDevice>,
    selectedAddress: String,
    onSelect: (BluetoothLink.BondedDevice) -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = Panel,
        titleContentColor = Dim,
        textContentColor = Cream,
        title = { Text("BT DEVICE", style = MaterialTheme.typography.labelSmall) },
        text = {
            // Scrollable: a long bonded-device list must not overflow the dialog.
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                if (devices.isEmpty()) {
                    Text(
                        "NO BONDED DEVICES — PAIR THE CONSOLE IN SYSTEM SETTINGS",
                        style = OpenAyDataStyles.micro,
                        color = Dim,
                    )
                }
                devices.forEach { device ->
                    val selected = device.address == selectedAddress
                    Column(
                        modifier = Modifier
                            .clip(RoundedCornerShape(2.dp))
                            .background(if (selected) Amber.copy(alpha = 0.10f) else Color.Transparent)
                            .then(if (selected) Modifier.border(1.dp, Amber, RoundedCornerShape(2.dp)) else Modifier)
                            .clickable { onSelect(device) }
                            .padding(horizontal = 10.dp, vertical = 8.dp),
                        verticalArrangement = Arrangement.spacedBy(2.dp),
                    ) {
                        Text(
                            device.name.ifBlank { "Unnamed device" },
                            style = OpenAyDataStyles.small,
                            color = if (selected) Amber else Cream,
                        )
                        Text(device.address, style = OpenAyDataStyles.micro, color = Dim)
                    }
                }
            }
        },
        confirmButton = {
            Text(
                "CANCEL",
                style = MaterialTheme.typography.labelMedium,
                color = Dim,
                modifier = Modifier
                    .clip(RoundedCornerShape(2.dp))
                    .clickable { onDismiss() }
                    .padding(horizontal = 10.dp, vertical = 6.dp),
            )
        },
    )
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