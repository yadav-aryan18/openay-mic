package com.openay.mic.ui.theme

import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import com.openay.mic.R

/** Chakra Petch — display/labels (hardware silkscreen character). */
val DisplayChakra = FontFamily(
    Font(R.font.chakra_petch_semibold, FontWeight.SemiBold),
    Font(R.font.chakra_petch_medium, FontWeight.Medium),
)

/** IBM Plex Mono — every number on screen (ports, rates, ms, IPs). */
val DataPlex = FontFamily(
    Font(R.font.ibm_plex_mono_regular, FontWeight.Normal),
    Font(R.font.ibm_plex_mono_medium, FontWeight.Medium),
)