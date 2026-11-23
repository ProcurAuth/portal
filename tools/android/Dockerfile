# Android build toolchain: JDK 17, Android SDK/NDK, Gradle, Rust Android targets.
# Used locally (tools/android/build-apk.sh) and by CI.
FROM eclipse-temurin:25-jdk-jammy

ARG CMDLINE_TOOLS=11076708
ARG ANDROID_PLATFORM=35
ARG BUILD_TOOLS=35.0.0
ARG NDK_VERSION=27.2.12479018
ARG GRADLE_VERSION=8.10.2
ARG RUST_VERSION=1.98.0

ENV ANDROID_HOME=/opt/android-sdk \
    ANDROID_SDK_ROOT=/opt/android-sdk \
    GRADLE_HOME=/opt/gradle \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo
ENV ANDROID_NDK_HOME=${ANDROID_HOME}/ndk/${NDK_VERSION} \
    PATH=/opt/cargo/bin:/opt/gradle/bin:/opt/android-sdk/cmdline-tools/latest/bin:${PATH}

RUN apt-get update \
 && apt-get install -y --no-install-recommends unzip curl ca-certificates git build-essential \
 && rm -rf /var/lib/apt/lists/*

RUN mkdir -p ${ANDROID_HOME}/cmdline-tools \
 && curl -fsSL -o /tmp/tools.zip https://dl.google.com/android/repository/commandlinetools-linux-${CMDLINE_TOOLS}_latest.zip \
 && unzip -q /tmp/tools.zip -d /tmp/tools \
 && mv /tmp/tools/cmdline-tools ${ANDROID_HOME}/cmdline-tools/latest \
 && rm -rf /tmp/tools /tmp/tools.zip \
 && yes | sdkmanager --licenses >/dev/null \
 && sdkmanager --install "platform-tools" "platforms;android-${ANDROID_PLATFORM}" \
      "build-tools;${BUILD_TOOLS}" "ndk;${NDK_VERSION}" >/dev/null

RUN curl -fsSL -o /tmp/gradle.zip https://services.gradle.org/distributions/gradle-${GRADLE_VERSION}-bin.zip \
 && unzip -q /tmp/gradle.zip -d /opt \
 && mv /opt/gradle-${GRADLE_VERSION} ${GRADLE_HOME} \
 && rm /tmp/gradle.zip

RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_VERSION} \
 && rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android \
 && chmod -R a+rwX /opt/cargo /opt/rustup

WORKDIR /src
