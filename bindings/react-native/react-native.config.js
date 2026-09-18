module.exports = {
  dependency: {
    platforms: {
      android: {
        sourceDir: './android',
        packageImportPath: 'import org.cashu.cdk.crypto.CdkReactNativePackage;',
        packageInstance: 'new CdkReactNativePackage()',
      },
      ios: { podspecPath: './CdkReactNative.podspec' },
    },
  },
};
