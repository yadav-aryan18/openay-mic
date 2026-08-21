cmake_minimum_required(VERSION 3.22.1)
project(openay_app LANGUAGES CXX)
# Path correction vs. the original Phase 3 spec (../../../native resolved to
# android/app/native, one level too shallow): the native tree lives at
# android/native, i.e. four levels up from src/main/cpp.
add_subdirectory(${CMAKE_CURRENT_SOURCE_DIR}/../../../../native ${CMAKE_CURRENT_BINARY_DIR}/openay-native)
