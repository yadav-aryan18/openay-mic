package com.openay.mic.ui.theme

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable

/**
 * OpenAY Mic theme — "studio rack at night". Dark-only by design (design.md);
 * every token derives from the binding palette. No dynamic color, no light
 * scheme: the rack is always on.
 */
private val OpenAyDarkColors = darkColorScheme(
    primary = Amber,
    onPrimary = Ink,
    primaryContainer = AmberContainer,
    onPrimaryContainer = Cream,
    inversePrimary = Amber,
    secondary = Dim,
    onSecondary = Ink,
    secondaryContainer = Panel,
    onSecondaryContainer = Cream,
    tertiary = Amber,
    onTertiary = Ink,
    tertiaryContainer = Panel,
    onTertiaryContainer = Cream,
    background = Ink,
    onBackground = Cream,
    surface = Ink,
    onSurface = Cream,
    surfaceVariant = Panel,
    onSurfaceVariant = Dim,
    surfaceTint = Amber,
    inverseSurface = Panel,
    inverseOnSurface = Cream,
    error = Tally,
    onError = Cream,
    errorContainer = TallyContainer,
    onErrorContainer = TallySoft,
    outline = Line,
    outlineVariant = Line,
    scrim = Ink,
    surfaceContainerLowest = Ink,
    surfaceContainerLow = Panel,
    surfaceContainer = Panel,
    surfaceContainerHigh = Panel,
    surfaceContainerHighest = PanelRaised,
)

@Composable
fun OpenAyTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = OpenAyDarkColors,
        typography = OpenAyTypography,
        content = content,
    )
}