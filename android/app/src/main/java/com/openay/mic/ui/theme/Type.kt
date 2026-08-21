package com.openay.mic.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

/**
 * Material slot mapping: Chakra Petch for display/labels (ALL-CAPS + wide
 * tracking baked into [chakraLabel] via 0.08em), Roboto default for body
 * (acceptable per design.md where Chakra is too wide). Plex Mono lives in
 * [OpenAyDataStyles] — every number on screen comes from there.
 */
val OpenAyTypography = Typography(
    displayLarge = chakraSemiBold(57.sp, 64.sp),
    displayMedium = chakraSemiBold(45.sp, 52.sp),
    displaySmall = chakraSemiBold(36.sp, 44.sp),
    headlineLarge = chakraSemiBold(32.sp, 40.sp),
    headlineMedium = chakraSemiBold(28.sp, 36.sp),
    headlineSmall = chakraSemiBold(24.sp, 32.sp),
    titleLarge = chakraSemiBold(22.sp, 28.sp),
    titleMedium = chakraSemiBold(16.sp, 24.sp),
    titleSmall = chakraSemiBold(14.sp, 20.sp),
    labelLarge = chakraLabel(14.sp, 20.sp),
    labelMedium = chakraLabel(12.sp, 16.sp),
    labelSmall = chakraLabel(11.sp, 14.sp),
)

private fun chakraSemiBold(fontSize: TextUnit, lineHeight: TextUnit) = TextStyle(
    fontFamily = DisplayChakra,
    fontWeight = FontWeight.SemiBold,
    fontSize = fontSize,
    lineHeight = lineHeight,
)

/** Labels are ALL-CAPS with wide (+8%) tracking, Chakra Petch Medium. */
private fun chakraLabel(fontSize: TextUnit, lineHeight: TextUnit) = TextStyle(
    fontFamily = DisplayChakra,
    fontWeight = FontWeight.Medium,
    fontSize = fontSize,
    lineHeight = lineHeight,
    letterSpacing = 0.08.em,
)

/** Plex Mono styles — every number on screen (ports, rates, ms, IPs). */
object OpenAyDataStyles {
    val large = mono(DataPlex, FontWeight.Medium, 18.sp, 24.sp)
    val regular = mono(DataPlex, FontWeight.Normal, 14.sp, 20.sp)
    val small = mono(DataPlex, FontWeight.Normal, 12.sp, 16.sp)
    val micro = mono(DataPlex, FontWeight.Normal, 10.sp, 14.sp)

    private fun mono(family: FontFamily, weight: FontWeight, fontSize: TextUnit, lineHeight: TextUnit) = TextStyle(
        fontFamily = family,
        fontWeight = weight,
        fontSize = fontSize,
        lineHeight = lineHeight,
    )
}

/** Wordmark — "OPENAY MIC" (caps, wide tracking). */
val WordmarkStyle = TextStyle(
    fontFamily = DisplayChakra,
    fontWeight = FontWeight.SemiBold,
    fontSize = 20.sp,
    lineHeight = 24.sp,
    letterSpacing = 0.10.em,
)