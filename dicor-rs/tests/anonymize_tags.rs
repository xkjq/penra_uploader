use dicor_rs::anonymize_file;
use dicom_object::{open_file, InMemDicomObject, FileMetaTableBuilder, Tag};
use dicom_core::header::VR;
use dicom_dictionary_std::uids;
use tempfile::tempdir;

fn make_test_file(path: &std::path::Path) {
    // create an in-memory DICOM and set a variety of tags
    let mut obj = InMemDicomObject::new_empty();

    // helper to set string values
    let mut set = |tag: Tag, vr: VR, val: &str| {
        let _ = obj.put_str(tag, vr, val);
    };

    // date/time tags
    set(Tag(0x0008,0x0020), VR::DA, "20200101"); // StudyDate
    set(Tag(0x0008,0x0021), VR::DA, "20200102"); // SeriesDate
    set(Tag(0x0008,0x0022), VR::DA, "20200103"); // AcquisitionDate
    set(Tag(0x0008,0x0023), VR::DA, "20200104"); // ContentDate
    set(Tag(0x0008,0x0024), VR::DA, "20200105"); // OverlayDate
    set(Tag(0x0008,0x0025), VR::DA, "20200106"); // CurveDate
    set(Tag(0x0008,0x002A), VR::DT, "20200101080000"); // AcquisitionDatetime
    set(Tag(0x0008,0x0030), VR::TM, "080000"); // StudyTime
    set(Tag(0x0008,0x0031), VR::TM, "090000"); // SeriesTime
    set(Tag(0x0008,0x0032), VR::TM, "100000"); // AcquisitionTime
    set(Tag(0x0008,0x0033), VR::TM, "110000"); // ContentTime
    set(Tag(0x0008,0x0034), VR::TM, "120000"); // OverlayTime
    set(Tag(0x0008,0x0035), VR::TM, "130000"); // CurveTime

    // text / id fields
    set(Tag(0x0008,0x0050), VR::SH, "ACC123"); // AccessionNumber
    set(Tag(0x0008,0x0080), VR::LO, "Institution X"); // InstitutionName
    set(Tag(0x0008,0x0081), VR::ST, "Addr"); // InstitutionAddress
    set(Tag(0x0008,0x0090), VR::PN, "Dr. Ref"); // ReferringPhysiciansName
    set(Tag(0x0008,0x0092), VR::ST, "RefAddr"); // ReferringPhysiciansAddress
    set(Tag(0x0008,0x0094), VR::LO, "0123456789"); // ReferringPhysiciansTelephoneNumber
    set(Tag(0x0008,0x1040), VR::LO, "Dept"); // InstitutionalDepartmentName
    set(Tag(0x0008,0x1048), VR::PN, "PhysRecord"); // PhysicianOfRecord
    set(Tag(0x0008,0x1050), VR::PN, "Performer"); // PerformingPhysiciansName
    set(Tag(0x0008,0x1060), VR::PN, "Reader"); // NameOfPhysicianReadingStudy
    set(Tag(0x0008,0x1070), VR::PN, "Operator"); // OperatorsName

    // patient fields
    set(Tag(0x0010,0x0010), VR::PN, "John^Doe"); // PatientsName
    set(Tag(0x0010,0x0020), VR::LO, "PAT123"); // PatientID
    set(Tag(0x0010,0x0021), VR::LO, "ISSUER"); // IssuerOfPatientID
    set(Tag(0x0010,0x0030), VR::DA, "19900101"); // PatientsBirthDate
    set(Tag(0x0010,0x0032), VR::TM, "070000"); // PatientsBirthTime
    set(Tag(0x0010,0x0040), VR::LO, "M"); // PatientsSex
    set(Tag(0x0010,0x1000), VR::LO, "OTHERIDS"); // OtherPatientIDs
    set(Tag(0x0010,0x1001), VR::PN, "OtherNames"); // OtherPatientNames
    set(Tag(0x0010,0x1005), VR::PN, "BirthName"); // PatientsBirthName
    set(Tag(0x0010,0x1010), VR::AS, "030Y"); // PatientsAge
    set(Tag(0x0010,0x1040), VR::LO, "Home Addr"); // PatientsAddress
    set(Tag(0x0010,0x1060), VR::PN, "MotherName"); // PatientsMothersBirthName
    set(Tag(0x0010,0x2150), VR::LO, "Country"); // CountryOfResidence
    set(Tag(0x0010,0x2152), VR::LO, "Region"); // RegionOfResidence
    set(Tag(0x0010,0x2154), VR::LO, "555-0000"); // PatientsTelephoneNumbers

    set(Tag(0x0020,0x0010), VR::SH, "STUDY1"); // StudyID

    set(Tag(0x0038,0x0300), VR::LO, "Room1"); // CurrentPatientLocation
    set(Tag(0x0038,0x0400), VR::LO, "InstResidence"); // PatientsInstitutionResidence

    // 0040 A1xx sequence tags
    set(Tag(0x0040,0xA120), VR::DT, "20200101120000"); // DateTime
    set(Tag(0x0040,0xA121), VR::DA, "20200101"); // Date
    set(Tag(0x0040,0xA122), VR::TM, "120000"); // Time
    set(Tag(0x0040,0xA123), VR::PN, "Person Name"); // PersonName

    // Ensure file meta/sop UIDs are present in dataset
    let _ = obj.put_str(Tag(0x0008,0x0016), VR::UI, "1.2.840.10008.5.1.4.1.1.1");
    let _ = obj.put_str(Tag(0x0008,0x0018), VR::UI, "2.25.123456789");

    // attach minimal file meta and write to file
    let file_obj = obj
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .media_storage_sop_class_uid("1.2.840.10008.5.1.4.1.1.1")
                .media_storage_sop_instance_uid("2.25.123456789"),
        )
        .expect("with_meta");
    let _ = file_obj.write_to_file(path).expect("write DICOM");
}

#[test]
fn tags_are_anonymised() {
    let tmp = tempdir().expect("tempdir");
    let in_path = tmp.path().join("in.dcm");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    make_test_file(&in_path);

    // call anonymizer
    let res = anonymize_file(&in_path, &out_dir, false, None).expect("anonymize");

    // open result
    let out_file_path = res;
    let obj = open_file(&out_file_path).expect("open result");

    // list of tags we expect to be changed or cleared
    let tags = vec![
        Tag(0x0008,0x0020), Tag(0x0008,0x0021), Tag(0x0008,0x0022), Tag(0x0008,0x0023),
        Tag(0x0008,0x0024), Tag(0x0008,0x0025), Tag(0x0008,0x002A), Tag(0x0008,0x0030),
        Tag(0x0008,0x0031), Tag(0x0008,0x0032), Tag(0x0008,0x0033), Tag(0x0008,0x0034),
        Tag(0x0008,0x0035), Tag(0x0008,0x0050), Tag(0x0008,0x0080), Tag(0x0008,0x0081),
        Tag(0x0008,0x0090), Tag(0x0008,0x0092), Tag(0x0008,0x0094), Tag(0x0008,0x0096),
        Tag(0x0008,0x1040), Tag(0x0008,0x1048), Tag(0x0008,0x1049), Tag(0x0008,0x1050),
        Tag(0x0008,0x1052), Tag(0x0008,0x1060), Tag(0x0008,0x1062), Tag(0x0008,0x1070),
        Tag(0x0010,0x0010), Tag(0x0010,0x0020), Tag(0x0010,0x0021), Tag(0x0010,0x0030),
        Tag(0x0010,0x0032), Tag(0x0010,0x0040), Tag(0x0010,0x1000), Tag(0x0010,0x1001),
        Tag(0x0010,0x1005), Tag(0x0010,0x1010), Tag(0x0010,0x1040), Tag(0x0010,0x1060),
        Tag(0x0010,0x2150), Tag(0x0010,0x2152), Tag(0x0010,0x2154), Tag(0x0020,0x0010),
        Tag(0x0038,0x0300), Tag(0x0038,0x0400), Tag(0x0040,0xA120), Tag(0x0040,0xA121),
        Tag(0x0040,0xA122), Tag(0x0040,0xA123),
    ];

    // open original and anonymised objects
    let orig_obj = open_file(&in_path).expect("open orig");
    for t in tags {
        if let Ok(orig_el) = orig_obj.element(t) {
            let orig = orig_el.to_str().map(|c| c.into_owned()).unwrap_or_else(|_| "".to_string());
            match obj.element(t) {
                Ok(new_el) => {
                    let new = new_el.to_str().map(|c| c.into_owned()).unwrap_or_else(|_| "".to_string());
                    assert!(new != orig, "Tag {:?} was not changed", t);
                }
                Err(_) => {
                    // element removed -> OK
                }
            }
        }
    }
}
