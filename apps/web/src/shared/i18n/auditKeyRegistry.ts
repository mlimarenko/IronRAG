import i18n from '@/shared/i18n';

export function registerConfigDrivenI18nKeysForAudit() {
  // Static calls for config-object keys consumed through t(key) lookups in excluded trees.
  void i18n.t(`graph.layoutDescriptions.${''}`);

  void i18n.t('login.purposeEmbedChunk');
  void i18n.t('login.purposeEmbedChunkDesc');
  void i18n.t('login.purposeExtractGraph');
  void i18n.t('login.purposeExtractGraphDesc');
  void i18n.t('login.purposeExtractText');
  void i18n.t('login.purposeExtractTextDesc');
  void i18n.t('login.purposeQueryAnswer');
  void i18n.t('login.purposeQueryAnswerDesc');
  void i18n.t('login.purposeQueryCompile');
  void i18n.t('login.purposeQueryCompileDesc');
  void i18n.t('login.purposeQueryRetrieve');
  void i18n.t('login.purposeQueryRetrieveDesc');
  void i18n.t('login.purposeVision');
  void i18n.t('login.purposeVisionDesc');
}
