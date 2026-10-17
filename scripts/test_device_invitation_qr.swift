// macOS independent QR decoder proof. Inputs must be explicit SYNTHETIC fixtures.
// Usage: swift scripts/test_device_invitation_qr.swift invitation.png invitation.json
import Foundation
import Vision

func verifyInvitationQR() throws {
        guard CommandLine.arguments.count == 3 else {
            throw NSError(domain:"Expected explicit synthetic PNG and JSON file paths",code:2)
        }
        let image=URL(fileURLWithPath:CommandLine.arguments[1])
        let expected=try Data(contentsOf:URL(fileURLWithPath:CommandLine.arguments[2]))
        let request=VNDetectBarcodesRequest();request.symbologies=[.qr]
        try VNImageRequestHandler(url:image,options:[:]).perform([request])
        guard let text=request.results?.first?.payloadStringValue,Data(text.utf8)==expected else {
            throw NSError(domain:"Independent QR decoder did not recover exact invitation bytes",code:1)
        }
        let object=try JSONSerialization.jsonObject(with:expected) as? [String:Any]
        guard object?["version"] as? Int == 2,object?["connection_type"] as? String == "pi-messages" else {
            throw NSError(domain:"Unexpected typed invitation fixture",code:1)
        }
        print("{\"decoded_exact_bytes\":true,\"version\":2,\"connection_type\":\"pi-messages\"}")
}
try verifyInvitationQR()
