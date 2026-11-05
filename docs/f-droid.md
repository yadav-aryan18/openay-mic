# F-Droid publication — status and submission steps

This app is being prepared for [F-Droid](https://f-droid.org). Everything the
reviewers need lives in this repo; the submission itself is a merge request to
F-Droid's packaging repo.

## What is already done

- **Offline-capable build** (the F-Droid buildbot's hard requirement):
  opus is vendored as a git submodule (`android/native/third_party/opus`,
  xiph/opus pinned to v1.5.2, BSD-3-Clause) and
  `android/native/CMakeLists.txt` compiles it via
  `add_subdirectory(third_party/opus)` — no `FetchContent` network download.
  Build servers cut the network during builds, so nothing may fetch at
  compile time. Gradle dependencies (Compose, Oboe via Maven) are fine as-is.
- **Store metadata** lives in the app repo and is read automatically by
  fdroidserver and the F-Droid client:
  `android/fastlane/metadata/android/com.openay.mic/en-US/`
  (title, short/full description, changelog for versionCode 5).
- **Packaging recipe** staged at `fdroid/metadata/com.openay.mic.yml`
  (copy of the file that goes into the fdroiddata merge request).
- All dependencies are FOSS (Apache-2.0 Compose/Oboe, BSD-3 opus, OFL fonts);
  the only binary in the tree is the standard Gradle wrapper jar, which F-Droid
  accepts.
- Verified locally: `./gradlew :app:assembleDebug` builds cleanly from the
  submodule (v0.3.3, versionCode 5).

## Remaining steps (submission)

1. **Tag and release v0.3.3** (versionCode 5) on GitHub — the recipe builds
   that tag.
2. Create a GitLab account, fork <https://gitlab.com/fdroid/fdroiddata>.
3. In the fork, add this repo's `fdroid/metadata/com.openay.mic.yml` as
   `metadata/com.openay.mic.yml`, commit on a branch like
   `add-com-openay-mic`.
4. Open a merge request on gitlab.com/fdroid/fdroiddata using the
   **"App inclusion"** MR template.
5. The check bot (`fdroidcheckbot`) runs lint/build checks automatically;
   iterate on any comments from volunteer packagers.
6. After merge, the nightly build publishes the app to the F-Droid client
   (can take a few days).

Optional local validation before the MR (needs the fdroidserver tools):
`pip install fdroidserver`, then `fdroid readmeta` inside the fdroiddata
checkout with the file in place.

## Future releases

`AutoUpdateMode: Version v%v` + `UpdateCheckMode: Tags` means new tags
(`v0.3.4`, ...) are picked up automatically; fdroidserver reads the
versionCode/versionName from `android/app/build.gradle.kts` at the tag and
inserts a build entry. Nothing manual unless the build recipe itself needs to
change.

## Notes

- Only `x86_64` and `arm64-v8a` ABIs are built (`abiFilters` in
  `build.gradle.kts`). Consider adding `armeabi-v7a` in a future release to
  cover older 32-bit phones.
- Screenshots for the store listing can be added later under
  `android/fastlane/metadata/android/com.openay.mic/en-US/images/phoneScreenshots/`.
- When editing this recipe, validate it with `fdroid readmeta` (Python YAML
  is not the same as fdroidserver's stricter parser).
