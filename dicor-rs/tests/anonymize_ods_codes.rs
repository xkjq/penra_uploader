use dicor_rs::anonymize_file;
use dicom_core::header::VR;
use dicom_core::value::{DataSetSequence, Value};
use dicom_core::{DataElement, Length};
use dicom_dictionary_std::uids;
use dicom_object::{FileMetaTableBuilder, InMemDicomObject, Tag};
use tempfile::tempdir;

/// ODS (Organisation Data Service) codes identify the issuing trust / GP
/// practice. They can appear in InstitutionCodeSequence as well as the
/// UniversalEntityID fields, so verify they are cleared from the output.
#[test]
fn ods_codes_are_removed() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("ods.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut obj = InMemDicomObject::new_empty();
    let _ = obj.put_str(Tag(0x0008, 0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008, 0x0018), VR::UI, "2.25.9999");
    let _ = obj.put_str(Tag(0x0010, 0x0010), VR::PN, "Doe^John");
    let _ = obj.put_str(Tag(0x0010, 0x0020), VR::LO, "PAT123");

    // InstitutionCodeSequence (0008,0082) with an ODS code.
    let mut code_item = InMemDicomObject::new_empty();
    let _ = code_item.put_str(Tag(0x0008, 0x0100), VR::SH, "RGT01"); // CodeValue
    let _ = code_item.put_str(Tag(0x0008, 0x0102), VR::SH, "ODS"); // CodingSchemeDesignator
    let _ = code_item.put_str(Tag(0x0008, 0x0104), VR::LO, "Penra Test Trust"); // CodeMeaning
    let _ = obj.put(DataElement::new(
        Tag(0x0008, 0x0082),
        VR::SQ,
        Value::Sequence(DataSetSequence::new(vec![code_item], Length::UNDEFINED)),
    ));

    // IssuerOfPatientIDQualifiersSequence (0010,0024) with ODS identifiers.
    let mut issuer_item = InMemDicomObject::new_empty();
    let _ = issuer_item.put_str(Tag(0x0040, 0x0032), VR::UT, "RGT01"); // UniversalEntityID
    let _ = issuer_item.put_str(Tag(0x0040, 0x0033), VR::CS, "ODS"); // UniversalEntityIDType
    let _ = obj.put(DataElement::new(
        Tag(0x0010, 0x0024),
        VR::SQ,
        Value::Sequence(DataSetSequence::new(vec![issuer_item], Length::UNDEFINED)),
    ));

    // Top-level ODS fields.
    let _ = obj.put_str(Tag(0x0040, 0x0031), VR::UT, "RGT01"); // LocalEntity
    let _ = obj.put_str(Tag(0x0040, 0x0032), VR::UT, "RGT01"); // UniversalEntityID
    let _ = obj.put_str(Tag(0x0040, 0x0033), VR::CS, "ODS"); // UniversalEntityIDType

    let file_obj = obj
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
                .media_storage_sop_instance_uid("2.25.9999"),
        )
        .expect("with_meta");
    file_obj.write_to_file(&in_path).expect("write input");

    let res = anonymize_file(&in_path, &out_dir, false, false, false, None).expect("anonymize");
    let out_bytes = std::fs::read(&res).expect("read output");
    let out_text = String::from_utf8_lossy(&out_bytes);

    assert!(!out_text.contains("RGT01"), "ODS code remained in output");
    assert!(!out_text.contains("ODS"), "ODS designator remained in output");
    assert!(
        !out_text.contains("Penra Test Trust"),
        "InstitutionCodeSequence meaning remained in output"
    );
}
